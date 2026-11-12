//! Scanner integration tests against a temp library (PLAN.md M2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;

use common::{SUBTITLE, TestEnv, VIDEOS};
use marathon_server::db::ItemRow;

const ITEM_COLUMNS: &str = "SELECT id, source_id, path, size, mtime, show, season, episode, \
                            title, duration_ms, subtitles_json, missing, external_id FROM items";

async fn items(app: &marathon_server::App) -> Vec<ItemRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{ITEM_COLUMNS} ORDER BY path")))
        .fetch_all(app.db().pool())
        .await
        .unwrap()
}

async fn item(app: &marathon_server::App, path: &str) -> ItemRow {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{ITEM_COLUMNS} WHERE path = ?"
    )))
    .bind(path)
    .fetch_one(app.db().pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn scan_creates_one_item_per_video_with_parsed_metadata() {
    let env = TestEnv::new();
    let app = env.app().await;

    let items = items(&app).await;
    assert_eq!(items.len(), VIDEOS.len());
    assert!(items.iter().all(|i| i.source_id == "lib" && !i.missing));

    let episodes: Vec<_> = items
        .iter()
        .filter(|i| i.path.contains("Season"))
        .map(|i| (i.show.as_deref(), i.season, i.episode, i.title.as_str()))
        .collect();
    assert_eq!(
        episodes,
        [
            (Some("Test Show"), Some(1), Some(1), "Pilot"),
            (Some("Test Show"), Some(1), Some(2), "The Second One"),
            (Some("Test Show"), Some(2), Some(1), "New Beginnings"),
            (Some("Test Show"), Some(2), Some(2), "Cliffhanger"),
        ]
    );

    let essay = item(&app, "Essays/Ep 10 - Difficulty Curves.mp4").await;
    assert_eq!(
        (essay.show, essay.season, essay.episode),
        (None, None, None)
    );
    assert_eq!(essay.title, "Ep 10 - Difficulty Curves");
    assert_eq!(essay.size, common::video_bytes(&essay.path).len() as i64);

    let unicode = item(&app, "Misc/Café Society's Ünïcode Clip — Ørsted.mkv").await;
    assert_eq!(unicode.title, "Café Society's Ünïcode Clip — Ørsted");
}

#[tokio::test]
async fn sidecar_subtitles_are_attached_to_their_video() {
    let env = TestEnv::new();
    let app = env.app().await;

    let pilot = item(&app, VIDEOS[0]).await;
    let subs: serde_json::Value = serde_json::from_str(&pilot.subtitles_json).unwrap();
    assert_eq!(
        subs,
        serde_json::json!([{ "path": SUBTITLE, "language": "en", "label": "en", "format": "srt" }])
    );
    // No other item picked it up.
    for other in &VIDEOS[1..] {
        assert_eq!(item(&app, other).await.subtitles_json, "[]", "{other}");
    }
}

#[tokio::test]
async fn deleted_files_are_marked_missing_and_restored_files_unmarked() {
    let env = TestEnv::new();
    let app = env.app().await;
    let gone = VIDEOS[1];
    let before = item(&app, gone).await;

    fs::remove_file(env.library().join(gone)).unwrap();
    let report = app.rescan().await.unwrap();
    assert_eq!(report.newly_missing, 1);
    assert_eq!(report.items_present as usize, VIDEOS.len() - 1);
    let missing = item(&app, gone).await;
    assert!(
        missing.missing,
        "deleted file must be marked missing, not removed"
    );
    assert_eq!(missing.id, before.id);
    assert_eq!(items(&app).await.len(), VIDEOS.len());

    // Rescanning again changes nothing.
    let report = app.rescan().await.unwrap();
    assert_eq!(
        (report.newly_missing, report.added, report.updated),
        (0, 0, 0)
    );

    env.write_video(gone);
    let report = app.rescan().await.unwrap();
    assert_eq!(report.restored, 1);
    let restored = item(&app, gone).await;
    assert!(!restored.missing);
    assert_eq!(restored.id, before.id, "a restored file keeps its item ID");
}

#[tokio::test]
async fn new_and_changed_files_are_picked_up() {
    let env = TestEnv::new();
    let app = env.app().await;

    env.write_video("TV/Test Show/Season 2/Test Show - S02E03 - Encore.mkv");
    let changed = VIDEOS[5];
    let new_content = b"different, longer content than before";
    fs::write(env.library().join(changed), new_content).unwrap();
    sqlx::query("UPDATE items SET duration_ms = 1234 WHERE path = ?")
        .bind(changed)
        .execute(app.db().pool())
        .await
        .unwrap();

    let report = app.rescan().await.unwrap();
    assert_eq!((report.added, report.updated), (1, 1));
    let row = item(&app, changed).await;
    assert_eq!(row.size, new_content.len() as i64);
    assert_eq!(
        row.duration_ms, None,
        "a changed file's duration is unknown again"
    );
}

#[tokio::test]
async fn unreadable_source_root_leaves_items_alone() {
    let env = TestEnv::new();
    let app = env.app().await;
    fs::rename(env.library(), env.dir.path().join("unmounted")).unwrap();

    let report = app.rescan().await.unwrap();
    assert_eq!(report.sources_scanned, 0);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(items(&app).await.iter().all(|i| !i.missing));
}

#[tokio::test]
async fn channel_membership_follows_selectors() {
    let env = TestEnv::new();
    let app = env.app().await;

    let members = |channel: &'static str| members(&app, channel);
    // Non-recursive: Season folders yes, Extras no.
    assert_eq!(members("show").await, VIDEOS[..4].to_vec());
    // Recursive: includes the subfolder.
    assert_eq!(members("essays").await.len(), 3);
    assert!(members("empty").await.is_empty());
}

/// The library paths of a channel's items, sorted.
async fn members(app: &marathon_server::App, channel: &str) -> Vec<String> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT i.path FROM channel_items ci JOIN items i ON i.id = ci.item_id
         WHERE ci.channel_id = ? ORDER BY i.path",
    )
    .bind(channel)
    .fetch_all(app.db().pool())
    .await
    .unwrap();
    rows.into_iter().map(|(p,)| p).collect()
}

#[tokio::test]
async fn files_and_exclude_pick_exactly_what_they_name() {
    let env = TestEnv::new();
    let channels = r#"
[[channel]]
id = "picked"
number = 1
name = "Picked"
items = [
  { source = "lib", path = "TV/Test Show", files = [
      "Season 2/Test Show - S02E02 - Cliffhanger.mkv",
      "Extras/Blooper Reel.mkv",
      "Season 1/No Such Episode.mkv",
  ] },
  { source = "lib", path = "Essays", recursive = true, exclude = ["Deep Dives"] },
]
rule = { kind = "listed" }
"#;
    let config =
        marathon_server::config::Config::from_toml_str(&env.config_toml("", channels)).unwrap();
    let app = marathon_server::App::open(config).await.unwrap();
    assert_eq!(
        members(&app, "picked").await,
        [
            "Essays/Ep 10 - Difficulty Curves.mp4",
            "Essays/Ep 2 - Pacing.mkv",
            "TV/Test Show/Extras/Blooper Reel.mkv",
            "TV/Test Show/Season 2/Test Show - S02E02 - Cliffhanger.mkv",
        ]
    );
}
