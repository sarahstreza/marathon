//! Config is upserted into the database on startup, and channel state
//! survives config edits and restarts (PLAN.md §7, M2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{CHANNELS, TestEnv, advance, now, path_of};
use marathon_server::App;
use marathon_server::config::Config;

async fn channel_rows(app: &App) -> Vec<(String, i64, String, String)> {
    sqlx::query_as("SELECT id, number, name, rule_json FROM channels ORDER BY number")
        .fetch_all(app.db().pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn config_channels_are_upserted_on_startup() {
    let env = TestEnv::new();
    let app = env.app().await;
    let rows = channel_rows(&app).await;
    let ids: Vec<_> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(ids, ["show", "essays", "seasons", "misc", "empty"]);
    assert_eq!(rows[0].3, r#"{"kind":"ordered"}"#);
    let sources: Vec<(String, String)> = sqlx::query_as("SELECT id, kind FROM sources")
        .fetch_all(app.db().pool())
        .await
        .unwrap();
    assert_eq!(sources, [("lib".to_owned(), "filesystem".to_owned())]);
}

#[tokio::test]
async fn editing_a_rule_and_restarting_keeps_channel_state() {
    let env = TestEnv::new();
    let app = env.app().await;

    // Move `show` along and save a position.
    advance(&app, "show").await;
    let before = now(&app, "show").await;
    assert_eq!(path_of(&before), common::VIDEOS[1]);
    let reply = common::post(&app, "/api/v1/channels/show/progress")
        .json(serde_json::json!({ "item_id": before["item"]["id"], "position_ms": 4242 }))
        .send()
        .await;
    assert_eq!(reply.status, 204);
    app.db().close().await;
    drop(app);

    // "Edit the config": `show` becomes shuffled and is renamed and
    // renumbered; `misc` is removed.
    let edited = CHANNELS
        .replacen(
            "number = 1\nname = \"Test Show\"\nitems = [{ source = \"lib\", path = \"TV/Test Show\" }]\nrule = { kind = \"ordered\" }",
            "number = 9\nname = \"Shuffled Show\"\nitems = [{ source = \"lib\", path = \"TV/Test Show\" }]\nrule = { kind = \"shuffled\" }",
            1,
        )
        .replace(
            "[[channel]]\nid = \"misc\"\nnumber = 4\nname = \"Misc\"\nitems = [{ source = \"lib\", path = \"Misc\" }]\nrule = { kind = \"ordered\" }\n",
            "",
        );
    assert_ne!(edited, CHANNELS, "test setup: replacement must apply");
    let config = Config::from_toml_str(&env.config_toml("", &edited)).unwrap();
    let app = App::open(config).await.unwrap();

    let rows = channel_rows(&app).await;
    let show = rows.iter().find(|r| r.0 == "show").unwrap();
    assert_eq!(show.1, 9);
    assert_eq!(show.2, "Shuffled Show");
    assert_eq!(
        show.3, r#"{"kind":"shuffled"}"#,
        "rule comes from the new config"
    );
    assert!(
        rows.iter().all(|r| r.0 != "misc"),
        "removed channels are dropped"
    );

    // Same item and position as before the restart.
    let after = now(&app, "show").await;
    assert_eq!(after["item"]["id"], before["item"]["id"]);
    assert_eq!(after["position_ms"], 4242);
    assert_eq!(after["channel_name"], "Shuffled Show");

    // The new rule takes over from there: the stored cursor becomes a
    // shuffle cursor, and within the rest of the switched-to cycle plus one
    // full cycle (at most 3 + 4 advances) every episode comes up.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..7 {
        seen.insert(path_of(&advance(&app, "show").await).to_owned());
    }
    assert_eq!(seen.len(), 4, "{seen:?}");
    let (cursor,): (String,) =
        sqlx::query_as("SELECT cursor_json FROM channel_state WHERE channel_id = 'show'")
            .fetch_one(app.db().pool())
            .await
            .unwrap();
    assert!(cursor.contains(r#""kind":"shuffled""#), "{cursor}");
}
