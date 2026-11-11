//! Nested channels (PLAN.md M11): channels whose entries include other
//! channels, with shared or independent child state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{TestEnv, get, item_id, path_of, post};
use marathon_server::App;
use marathon_server::config::Config;
use serde_json::{Value, json};

/// Videos for the nested tests, on top of the common test library.
const A: [&str; 3] = ["Nest/A/A 1.mkv", "Nest/A/A 2.mkv", "Nest/A/A 3.mkv"];
const B: [&str; 3] = ["Nest/B/B 1.mkv", "Nest/B/B 2.mkv", "Nest/B/B 3.mkv"];
const MISC: [&str; 2] = ["Nest/Misc/M 1.mkv", "Nest/Misc/M 2.mkv"];

/// Channels over those videos:
///
/// - `a` (hidden, no number) and `b` (21): ordered leaf channels.
/// - `parent` (20): `a` then `b`, ordered, sharing their state.
/// - `indie` (22): the same, keeping its own state for them.
/// - `mixed` (23): `a` plus the Misc folder's items, ordered.
/// - `e1`…`e6` (hidden): channels with nothing to play.
/// - `some-empty` (24): `e1` then `a`; `all-empty` (25): `e1`, `e2`;
///   `many-empty` (26): `e1`…`e6`.
/// - `sa`, `sb` (hidden): shuffled leaves; `shuffled-indie` (27) and
///   `weighted` (28) nest them with randomness at every level.
/// - `grand` (29): `parent` and `b`, so `b` is reached two ways (a
///   diamond), and `a` is a grandchild.
const NESTED: &str = r#"
[[channel]]
id = "a"
name = "A"
hidden = true
items = [{ source = "lib", path = "Nest/A" }]
rule = { kind = "ordered" }

[[channel]]
id = "b"
number = 21
name = "B"
items = [{ source = "lib", path = "Nest/B" }]
rule = { kind = "ordered" }

[[channel]]
id = "parent"
number = 20
name = "Parent"
items = [{ channel = "a" }, { channel = "b" }]
rule = { kind = "ordered" }

[[channel]]
id = "indie"
number = 22
name = "Indie"
items = [{ channel = "a" }, { channel = "b" }]
rule = { kind = "ordered" }
state = "independent"

[[channel]]
id = "mixed"
number = 23
name = "Mixed"
items = [{ channel = "a" }, { source = "lib", path = "Nest/Misc" }]
rule = { kind = "ordered" }

[[channel]]
id = "some-empty"
number = 24
name = "Some Empty"
items = [{ channel = "e1" }, { channel = "a" }]
rule = { kind = "ordered" }

[[channel]]
id = "all-empty"
number = 25
name = "All Empty"
items = [{ channel = "e1" }, { channel = "e2" }]
rule = { kind = "ordered" }

[[channel]]
id = "many-empty"
number = 26
name = "Many Empty"
items = [
  { channel = "e1" }, { channel = "e2" }, { channel = "e3" },
  { channel = "e4" }, { channel = "e5" }, { channel = "e6" },
]
rule = { kind = "ordered" }

[[channel]]
id = "sa"
name = "Shuffled A"
hidden = true
items = [{ source = "lib", path = "Nest/A" }]
rule = { kind = "shuffled" }

[[channel]]
id = "sb"
name = "Shuffled B"
hidden = true
items = [{ source = "lib", path = "Nest/B" }]
rule = { kind = "shuffled" }

[[channel]]
id = "shuffled-indie"
number = 27
name = "Shuffled Indie"
items = [{ channel = "sa" }, { channel = "sb" }, { source = "lib", path = "Nest/Misc" }]
rule = { kind = "shuffled" }
state = "independent"

[[channel]]
id = "weighted"
number = 28
name = "Weighted"
items = [{ channel = "sa", weight = 3 }, { channel = "sb" }]
rule = { kind = "weighted", by = "manual" }

[[channel]]
id = "grand"
number = 29
name = "Grand"
items = [{ channel = "parent" }, { channel = "b" }]
rule = { kind = "ordered" }
"#;

fn empty_channels() -> String {
    (1..=6)
        .map(|i| {
            format!(
                "[[channel]]\nid = \"e{i}\"\nname = \"Empty {i}\"\nhidden = true\n\
                 items = [{{ source = \"lib\", path = \"Nothing Here\" }}]\n\
                 rule = {{ kind = \"ordered\" }}\n"
            )
        })
        .collect()
}

fn nested_env() -> TestEnv {
    let env = TestEnv::new();
    for video in A.iter().chain(&B).chain(&MISC) {
        env.write_video(video);
    }
    env
}

fn nested_config(env: &TestEnv) -> Config {
    let channels = format!("{}{NESTED}{}", common::CHANNELS, empty_channels());
    Config::from_toml_str(&env.config_toml("", &channels)).unwrap()
}

async fn nested_app(env: &TestEnv) -> App {
    App::open(nested_config(env)).await.unwrap()
}

async fn now(app: &App, channel: &str) -> Value {
    common::now(app, channel).await
}

/// Advances (or skips) from the channel's current item.
async fn step(app: &App, channel: &str, reason: &str) -> Value {
    let current = now(app, channel).await;
    let reply = post(app, &format!("/api/v1/channels/{channel}/advance"))
        .json(json!({ "from_item_id": current["item"]["id"], "reason": reason }))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", body(&reply.body));
    reply.json()
}

async fn progress(app: &App, channel: &str, item: &Value, position_ms: u64) {
    let reply = post(app, &format!("/api/v1/channels/{channel}/progress"))
        .json(json!({ "item_id": item, "position_ms": position_ms }))
        .send()
        .await;
    assert_eq!(
        reply.status,
        StatusCode::NO_CONTENT,
        "{}",
        body(&reply.body)
    );
}

fn body(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The resolution path of a `now` (or summary) as channel IDs.
fn path_ids(value: &Value) -> Vec<String> {
    value["resolution_path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect()
}

/// A channel's own `channel_state` row as `(item_id, position_ms,
/// cursor_json)`, or `None` if it has none.
async fn state_row(app: &App, channel: &str) -> Option<(Option<String>, i64, String)> {
    app.db()
        .state(channel)
        .await
        .unwrap()
        .map(|s| (s.item_id, s.position_ms, s.cursor_json))
}

#[tokio::test]
async fn shared_state_is_one_position_through_parent_or_child() {
    let env = nested_env();
    let app = nested_app(&env).await;

    // Through the parent: it plays its first entry, child `a`.
    let through_parent = now(&app, "parent").await;
    assert_eq!(path_of(&through_parent), A[0]);
    assert_eq!(path_ids(&through_parent), ["parent", "a"]);
    assert_eq!(through_parent["resolution_path"][1]["name"], "A");
    assert_eq!(through_parent["channel_id"], "parent");
    progress(&app, "parent", &through_parent["item"]["id"], 30_000).await;

    // The progress landed in the child's own state: tuning the child
    // directly shows the same item and position.
    let (item, position, _) = state_row(&app, "a").await.unwrap();
    assert_eq!(item.as_deref(), through_parent["item"]["id"].as_str());
    assert_eq!(position, 30_000);
    let direct = now(&app, "a").await;
    assert_eq!(path_of(&direct), A[0]);
    assert_eq!(direct["position_ms"], 30_000);
    assert_eq!(path_ids(&direct), ["a"]);

    // And the other way round.
    progress(&app, "a", &direct["item"]["id"], 45_000).await;
    let back = now(&app, "parent").await;
    assert_eq!(path_of(&back), A[0]);
    assert_eq!(back["position_ms"], 45_000);

    // Finishing the item through the parent advances the child, then the
    // parent's rule picks its next entry, `b`.
    let next = step(&app, "parent", "ended").await;
    assert_eq!(path_of(&next), B[0]);
    assert_eq!(path_ids(&next), ["parent", "b"]);
    let direct = now(&app, "a").await;
    assert_eq!(path_of(&direct), A[1]);
    assert_eq!(direct["position_ms"], 0);
}

#[tokio::test]
async fn resuming_a_child_left_mid_item_resumes_mid_item() {
    let env = nested_env();
    let app = nested_app(&env).await;
    // Watch some of B 1 directly on `b`.
    let b = now(&app, "b").await;
    progress(&app, "b", &b["item"]["id"], 12_000).await;
    // The parent reaches `b` after `a`, and picks it up where it was.
    now(&app, "parent").await;
    let at_b = step(&app, "parent", "ended").await;
    assert_eq!(path_of(&at_b), B[0]);
    assert_eq!(at_b["position_ms"], 12_000);
}

#[tokio::test]
async fn skipping_on_the_parent_moves_the_child_and_the_parent() {
    let env = nested_env();
    let app = nested_app(&env).await;
    assert_eq!(path_of(&now(&app, "parent").await), A[0]);

    // NextItem: the child moves past the skipped item, and the parent's
    // rule picks the next entry.
    let skipped = step(&app, "parent", "skipped").await;
    assert_eq!(path_of(&skipped), B[0]);
    assert_eq!(path_of(&now(&app, "a").await), A[1]);
    // Back to `a`, which carries on from where the skip left it.
    let again = step(&app, "parent", "skipped").await;
    assert_eq!(path_of(&again), A[1]);
    assert_eq!(path_of(&now(&app, "b").await), B[1]);

    let history: Vec<Value> = get(&app, "/api/v1/history").send().await.json();
    let skips: Vec<&Value> = history.iter().filter(|e| e["kind"] == "skipped").collect();
    assert_eq!(skips.len(), 2);
    // Newest first: the skip of B 1, then of A 1.
    assert_eq!(skips[0]["channel_id"], "parent");
    assert_eq!(skips[0]["resolved_channel_id"], "b");
    assert_eq!(skips[0]["resolution_path"], json!(["parent", "b"]));
    assert_eq!(skips[1]["resolved_channel_id"], "a");
}

#[tokio::test]
async fn previous_on_the_parent_undoes_an_advance() {
    let env = nested_env();
    let app = nested_app(&env).await;
    let first = now(&app, "parent").await;
    let second = step(&app, "parent", "ended").await;
    assert_eq!(path_of(&second), B[0]);
    let reply = post(&app, "/api/v1/channels/parent/previous")
        .json(json!({ "from_item_id": second["item"]["id"] }))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let back: Value = reply.json();
    assert_eq!(back["item"]["id"], first["item"]["id"]);
    assert_eq!(path_of(&now(&app, "a").await), A[0]);
}

#[tokio::test]
async fn advance_through_a_parent_is_idempotent() {
    let env = nested_env();
    let app = nested_app(&env).await;
    let first = now(&app, "parent").await;
    let body = json!({ "from_item_id": first["item"]["id"], "reason": "ended" });
    let a: Value = post(&app, "/api/v1/channels/parent/advance")
        .json(body.clone())
        .send()
        .await
        .json();
    let b: Value = post(&app, "/api/v1/channels/parent/advance")
        .json(body)
        .send()
        .await
        .json();
    assert_eq!(path_of(&a), B[0]);
    assert_eq!(a["item"]["id"], b["item"]["id"]);
    // The child moved on exactly once.
    assert_eq!(path_of(&now(&app, "a").await), A[1]);
    let history: Vec<Value> = get(&app, "/api/v1/history").send().await.json();
    assert_eq!(history.iter().filter(|e| e["kind"] == "ended").count(), 1);
}

#[tokio::test]
async fn independent_state_never_touches_the_child_and_survives_restart() {
    let env = nested_env();
    let config = nested_config(&env);
    let app = App::open(config.clone()).await.unwrap();

    // Give `a` some state of its own first.
    let direct = now(&app, "a").await;
    progress(&app, "a", &direct["item"]["id"], 7_000).await;
    let a_before = state_row(&app, "a").await;
    let b_before = state_row(&app, "b").await;
    assert!(a_before.is_some());
    assert_eq!(b_before, None);

    // Play through `indie`: its own position in `a` starts from scratch.
    let first = now(&app, "indie").await;
    assert_eq!(path_of(&first), A[0]);
    assert_eq!(first["position_ms"], 0);
    assert_eq!(path_ids(&first), ["indie", "a"]);
    progress(&app, "indie", &first["item"]["id"], 20_000).await;
    let second = step(&app, "indie", "ended").await;
    assert_eq!(path_of(&second), B[0]);
    progress(&app, "indie", &second["item"]["id"], 9_000).await;
    let third = step(&app, "indie", "skipped").await;
    assert_eq!(path_of(&third), A[1]);
    progress(&app, "indie", &third["item"]["id"], 3_000).await;

    // The children's own state is exactly as it was.
    assert_eq!(state_row(&app, "a").await, a_before);
    assert_eq!(state_row(&app, "b").await, b_before);
    assert_eq!(now(&app, "a").await["position_ms"], 7_000);

    // Restart: `indie` remembers its own place in each child.
    app.db().close().await;
    drop(app);
    let app = App::open(config).await.unwrap();
    let resumed = now(&app, "indie").await;
    assert_eq!(path_of(&resumed), A[1]);
    assert_eq!(resumed["position_ms"], 3_000);
    let at_b = step(&app, "indie", "ended").await;
    assert_eq!(path_of(&at_b), B[1]);
    assert_eq!(state_row(&app, "b").await, None);
}

#[tokio::test]
async fn mixed_entries_follow_the_parents_rule() {
    let env = nested_env();
    let app = nested_app(&env).await;
    let mut seen = vec![path_of(&now(&app, "mixed").await).to_owned()];
    for _ in 0..5 {
        seen.push(path_of(&step(&app, "mixed", "ended").await).to_owned());
    }
    // Ordered over [channel a, M 1, M 2]: the child entry sorts first.
    assert_eq!(seen, [A[0], MISC[0], MISC[1], A[1], MISC[0], MISC[1]]);
    let item = now(&app, "mixed").await;
    assert_eq!(path_ids(&item), ["mixed"]);
}

#[tokio::test]
async fn empty_children_are_skipped_with_a_warning() {
    let env = nested_env();
    let app = nested_app(&env).await;
    // Ordered over [a, e1] (child entries sort by ID).
    let first = now(&app, "some-empty").await;
    assert_eq!(path_of(&first), A[0]);
    assert_eq!(first["warnings"], json!([]));

    // Advancing lands on `e1`, which has nothing to play: it is skipped,
    // with a warning for the client to report, and `a` plays on.
    let next = step(&app, "some-empty", "ended").await;
    assert_eq!(path_of(&next), A[1]);
    let warnings = next["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].as_str().unwrap().contains("\"e1\""),
        "{warnings:?}"
    );

    // The parent moved past the empty child, so tuning again is quiet.
    let again = now(&app, "some-empty").await;
    assert_eq!(again["warnings"], json!([]));
    assert_eq!(path_of(&again), A[1]);
}

#[tokio::test]
async fn a_parent_of_only_empty_children_stops() {
    let env = nested_env();
    let app = nested_app(&env).await;

    let reply = get(&app, "/api/v1/channels/all-empty/now").send().await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.error_code(), "channel_empty");
    let message = reply.json::<Value>()["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("\"e1\"") && message.contains("\"e2\""),
        "{message}"
    );

    // Six empty children: the failure cap stops it after five.
    let reply = get(&app, "/api/v1/channels/many-empty/now").send().await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    let message = reply.json::<Value>()["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(message.contains("stopped after skipping 5"), "{message}");

    // Both are listed as having nothing on.
    let channels: Vec<Value> = get(&app, "/api/v1/channels").send().await.json();
    let all_empty = channels.iter().find(|c| c["id"] == "all-empty").unwrap();
    assert_eq!(all_empty["current"], Value::Null);
    assert_eq!(all_empty["item_count"], 0);
}

#[tokio::test]
async fn hidden_channels_are_listed_only_on_request_but_can_be_tuned() {
    let env = nested_env();
    let app = nested_app(&env).await;
    let channels: Vec<Value> = get(&app, "/api/v1/channels").send().await.json();
    let ids: Vec<&str> = channels.iter().map(|c| c["id"].as_str().unwrap()).collect();
    for hidden in ["a", "sa", "sb", "e1"] {
        assert!(!ids.contains(&hidden), "{hidden} listed: {ids:?}");
    }
    let numbers: Vec<u64> = channels
        .iter()
        .map(|c| c["number"].as_u64().unwrap())
        .collect();
    let mut sorted = numbers.clone();
    sorted.sort_unstable();
    assert_eq!(numbers, sorted, "guide order is by number");
    assert!(channels.iter().all(|c| c["hidden"] == false));

    let parent = channels.iter().find(|c| c["id"] == "parent").unwrap();
    assert_eq!(path_ids(parent), ["parent", "a"]);
    assert_eq!(parent["item_count"], 6);

    let all: Vec<Value> = get(&app, "/api/v1/channels?include_hidden=true")
        .send()
        .await
        .json();
    let a = all.iter().find(|c| c["id"] == "a").unwrap();
    assert_eq!(a["hidden"], true);
    assert_eq!(a["number"], 0);
    // Hidden channels come after the others.
    let first_hidden = all.iter().position(|c| c["hidden"] == true).unwrap();
    assert!(all[first_hidden..].iter().all(|c| c["hidden"] == true));
    assert_eq!(all.len(), channels.len() + 9);

    // Tuning by ID works as for any channel.
    assert_eq!(path_of(&now(&app, "a").await), A[0]);
}

#[tokio::test]
async fn grandchildren_and_diamonds_resolve() {
    let env = nested_env();
    let app = nested_app(&env).await;
    // Ordered over [b, parent] (child entries sort by ID).
    let first = now(&app, "grand").await;
    assert_eq!(path_ids(&first), ["grand", "b"]);
    assert_eq!(path_of(&first), B[0]);
    // `b` moves on, and `grand` moves to `parent`, which plays its child
    // `a`: a grandchild.
    let second = step(&app, "grand", "ended").await;
    assert_eq!(path_ids(&second), ["grand", "parent", "a"]);
    assert_eq!(path_of(&second), A[0]);
    // `parent` moves `a` on and goes to `b`, and `grand` goes back to `b`
    // directly: the same channel, reached two ways, at one position.
    let third = step(&app, "grand", "ended").await;
    assert_eq!(path_ids(&third), ["grand", "b"]);
    assert_eq!(path_of(&third), B[1]);
    let parent = now(&app, "parent").await;
    assert_eq!(path_ids(&parent), ["parent", "b"]);
    assert_eq!(path_of(&parent), B[1]);

    // The latest `ended` is A 1's, which came through `parent`.
    let history: Vec<Value> = get(&app, "/api/v1/history").send().await.json();
    let ended = history.iter().find(|e| e["kind"] == "ended").unwrap();
    assert_eq!(ended["channel_id"], "grand");
    assert_eq!(ended["resolved_channel_id"], "a");
    assert_eq!(ended["resolution_path"], json!(["grand", "parent", "a"]));
}

/// The item IDs `GET plan` lists for a channel.
async fn plan(app: &App, channel: &str, query: &str) -> Vec<Value> {
    let reply = get(app, &format!("/api/v1/channels/{channel}/plan?{query}"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", body(&reply.body));
    reply.json()
}

/// Plays a channel live for `n` items: tune in, then advance.
async fn play(app: &App, channel: &str, n: usize) -> Vec<(Value, Vec<String>)> {
    let first = now(app, channel).await;
    let mut out = vec![(first["item"]["id"].clone(), path_ids(&first))];
    while out.len() < n {
        let next = step(app, channel, "ended").await;
        out.push((next["item"]["id"].clone(), path_ids(&next)));
    }
    out
}

#[tokio::test]
async fn a_plan_matches_live_playback() {
    for channel in ["shuffled-indie", "weighted", "grand", "mixed"] {
        let env = nested_env();
        let app = nested_app(&env).await;
        let planned = plan(&app, channel, "items=25").await;
        assert_eq!(planned.len(), 25, "{channel}");
        assert_eq!(planned[0]["starts_in_ms"], 0);
        // Planning moves nothing: tuning in finds the plan's first item.
        let live = play(&app, channel, 25).await;
        let planned: Vec<(Value, Vec<String>)> = planned
            .iter()
            .map(|p| (p["item"]["id"].clone(), path_ids(p)))
            .collect();
        assert_eq!(planned, live, "{channel}");
    }
}

#[tokio::test]
async fn a_plan_mid_way_matches_too_and_now_predicts_next() {
    let env = nested_env();
    let app = nested_app(&env).await;
    play(&app, "weighted", 7).await;
    let planned = plan(&app, "weighted", "items=12").await;
    let current = now(&app, "weighted").await;
    assert_eq!(current["next"]["item"]["id"], planned[1]["item"]["id"]);
    let live = play(&app, "weighted", 12).await;
    let ids: Vec<&Value> = planned.iter().map(|p| &p["item"]["id"]).collect();
    let live_ids: Vec<&Value> = live.iter().map(|(id, _)| id).collect();
    assert_eq!(ids, live_ids);
}

#[tokio::test]
async fn plans_by_hours_use_durations() {
    let env = nested_env();
    let app = nested_app(&env).await;
    // Known durations: 10 minutes each for A; B is unknown (30 minutes).
    for path in A {
        let id = item_id(&app, path).await;
        app.db().set_duration(&id, 600_000).await.unwrap();
    }
    let first = now(&app, "parent").await;
    progress(&app, "parent", &first["item"]["id"], 60_000).await;
    let planned = plan(&app, "parent", "hours=1").await;
    let starts: Vec<u64> = planned
        .iter()
        .map(|p| p["starts_in_ms"].as_u64().unwrap())
        .collect();
    // A 1 (9 minutes left), B 1 (30), A 2 (10), B 2 (30): B 2 starts at
    // 49 minutes; A 3 would start at 79, past the hour, so the plan ends.
    assert_eq!(starts, [0, 540_000, 2_340_000, 2_940_000], "{planned:?}");
    let reply = get(&app, "/api/v1/channels/nope/plan").send().await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let reply = get(&app, "/api/v1/channels/parent/plan?items=lots")
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn weighted_children_follow_their_weights() {
    let env = nested_env();
    let app = nested_app(&env).await;
    let planned = plan(&app, "weighted", "items=400").await;
    let from_a = planned.iter().filter(|p| path_ids(p)[1] == "sa").count();
    // Weights 3:1 over two entries can't avoid repeats, so `sa` gets as
    // close to its share as alternating allows: every other pick.
    assert!((190..=210).contains(&from_a), "{from_a} of 400 from sa");
    for pair in planned.windows(2) {
        assert_ne!(path_ids(&pair[0])[1], path_ids(&pair[1])[1]);
    }
}
