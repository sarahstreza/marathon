//! Channels that list their files explicitly and play them with the
//! `listed` rule: membership positions flow from config through
//! `channel_items` to the rules engine.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{TestEnv, VIDEOS, advance, now, path_of};
use marathon_server::App;
use marathon_server::config::Config;

const S1E1: &str = "TV/Test Show/Season 1/Test Show - S01E01 - Pilot.mkv";
const S1E2: &str = "TV/Test Show/Season 1/Test Show - S01E02 - The Second One.mkv";
const S2E1: &str = "TV/Test Show/Season 2/Test Show - S02E01 - New Beginnings.mkv";
const S2E2: &str = "TV/Test Show/Season 2/Test Show - S02E02 - Cliffhanger.mkv";
const EP2: &str = "Essays/Ep 2 - Pacing.mkv";
const EP10: &str = "Essays/Ep 10 - Difficulty Curves.mp4";

/// A channel playing season 2 before season 1's pilot, then the essays
/// folder (as one entry, in natural order), with S01E02 left out.
fn channel(files: &str, rule: &str) -> String {
    format!(
        r#"
[[channel]]
id = "mix"
number = 1
name = "Mix"
items = [
  {{ source = "lib", path = "TV/Test Show", files = [{files}] }},
  {{ source = "lib", path = "Essays", exclude = ["Deep Dives"] }},
]
rule = {{ kind = "{rule}" }}
"#
    )
}

const RUNNING_ORDER: &str = r#""Season 2/Test Show - S02E01 - New Beginnings.mkv",
    "Season 2/Test Show - S02E02 - Cliffhanger.mkv",
    "Season 1/Test Show - S01E01 - Pilot.mkv""#;

async fn open(env: &TestEnv, channels: &str) -> App {
    let config = Config::from_toml_str(&env.config_toml("", channels)).unwrap();
    App::open(config).await.unwrap()
}

/// The paths `mix` plays over `n` items, starting from where it is.
async fn walk(app: &App, n: usize) -> Vec<String> {
    walk_channel(app, "mix", n).await
}

/// The paths a channel plays over `n` items, starting from where it is.
async fn walk_channel(app: &App, channel: &str, n: usize) -> Vec<String> {
    let mut paths = vec![path_of(&now(app, channel).await).to_owned()];
    while paths.len() < n {
        paths.push(path_of(&advance(app, channel).await).to_owned());
    }
    paths
}

#[tokio::test]
async fn listed_channel_plays_in_config_order() {
    let env = TestEnv::new();
    let app = open(&env, &channel(RUNNING_ORDER, "listed")).await;
    assert_eq!(
        walk(&app, 6).await,
        [S2E1, S2E2, S1E1, EP2, EP10, S2E1],
        "listed order, then loops; S01E02 and Deep Dives never play"
    );
}

#[tokio::test]
async fn ordered_channel_ignores_list_order() {
    let env = TestEnv::new();
    let app = open(&env, &channel(RUNNING_ORDER, "ordered")).await;
    // Unparsed essays (no show) sort first, then by season and episode.
    assert_eq!(walk(&app, 5).await, [EP2, EP10, S1E1, S2E1, S2E2]);
    assert!(!walk(&app, 6).await.contains(&S1E2.to_owned()));
}

#[tokio::test]
async fn editing_the_list_and_restarting_follows_the_new_order() {
    let env = TestEnv::new();
    let app = open(&env, &channel(RUNNING_ORDER, "listed")).await;
    assert_eq!(walk(&app, 2).await, [S2E1, S2E2]);
    app.db().close().await;
    drop(app);

    // S02E02 moves to the end of the show's list, and S01E02 is added
    // before it.
    let edited = r#""Season 2/Test Show - S02E01 - New Beginnings.mkv",
        "Season 1/Test Show - S01E01 - Pilot.mkv",
        "Season 1/Test Show - S01E02 - The Second One.mkv",
        "Season 2/Test Show - S02E02 - Cliffhanger.mkv""#;
    let app = open(&env, &channel(edited, "listed")).await;
    assert_eq!(
        walk(&app, 4).await,
        [S2E2, EP2, EP10, S2E1],
        "resumes at S02E02 and carries on from its new place"
    );
    assert_eq!(walk(&app, 4).await[1..], [S1E1, S1E2, S2E2]);
}

#[tokio::test]
async fn listed_parent_plays_child_channels_in_config_order() {
    let env = TestEnv::new();
    // Child channels listed against ID order (`zeta` before `alpha`), with
    // a single listed file between them.
    let channels = r#"
[[channel]]
id = "parent"
number = 1
name = "Parent"
items = [
  { channel = "zeta" },
  { source = "lib", path = "TV/Test Show", files = [
      "Season 2/Test Show - S02E01 - New Beginnings.mkv",
  ] },
  { channel = "alpha" },
]
rule = { kind = "listed" }

[[channel]]
id = "zeta"
name = "Zeta"
hidden = true
items = [{ source = "lib", path = "Essays" }]
rule = { kind = "ordered" }

[[channel]]
id = "alpha"
name = "Alpha"
hidden = true
items = [{ source = "lib", path = "Misc" }]
rule = { kind = "ordered" }
"#;
    let app = open(&env, channels).await;
    let misc = VIDEOS[8];
    assert_eq!(
        walk_channel(&app, "parent", 6).await,
        [EP2, S2E1, misc, EP10, S2E1, misc],
        "each child plays its own next item when its turn comes round"
    );
}
