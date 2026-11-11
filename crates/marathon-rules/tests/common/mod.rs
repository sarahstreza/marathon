//! Helpers shared by the rules integration tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use marathon_rules::{Cursor, GroupBy, ItemKey, Order, Rule, Step, next, previous};

/// An episode of "Show" at `Show/Season {season}/S{season}E{episode}.mkv`.
pub fn episode(season: u32, episode: u32) -> ItemKey {
    ItemKey {
        id: format!("s{season}e{episode}"),
        path: format!("Show/Season {season}/S{season:02}E{episode:02}.mkv"),
        show: Some("Show".into()),
        season: Some(season),
        episode: Some(episode),
        position: None,
    }
}

/// An item with no parsed metadata.
pub fn clip(id: &str, path: &str) -> ItemKey {
    ItemKey {
        id: id.into(),
        path: path.into(),
        show: None,
        season: None,
        episode: None,
        position: None,
    }
}

/// `item` at list position `position`, for the `listed` rule.
pub fn at(position: u32, item: ItemKey) -> ItemKey {
    ItemKey {
        position: Some(position),
        ..item
    }
}

/// An entry standing for the child channel `id`, shaped the way
/// `marathon-server` builds them for nested channels (PLAN.md M11). To the
/// rules engine it is just another item.
pub fn channel_entry(id: &str) -> ItemKey {
    ItemKey {
        id: format!("channel:{id}"),
        path: format!("channels/{id}"),
        show: None,
        season: None,
        episode: None,
        position: None,
    }
}

/// `seasons` seasons of `per_season` episodes each.
pub fn show(seasons: u32, per_season: u32) -> Vec<ItemKey> {
    (1..=seasons)
        .flat_map(|s| (1..=per_season).map(move |e| episode(s, e)))
        .collect()
}

/// Every rule the engine supports.
pub fn all_rules() -> Vec<Rule> {
    let orders = [Order::Ordered, Order::Shuffled];
    let mut rules = vec![Rule::Ordered, Rule::Listed, Rule::Shuffled];
    for by in [GroupBy::Season, GroupBy::Folder] {
        for groups in orders {
            for within in orders {
                rules.push(Rule::Grouped { by, groups, within });
            }
        }
    }
    rules
}

/// Calls `next` `n` times starting from `cursor`, returning every step.
pub fn walk(rule: &Rule, items: &[ItemKey], cursor: Option<Cursor>, n: usize) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::with_capacity(n);
    let mut cursor = cursor;
    for _ in 0..n {
        let step = next(rule, items, cursor.as_ref()).expect("non-empty items give a step");
        cursor = Some(step.cursor.clone());
        steps.push(step);
    }
    steps
}

/// The item IDs of `steps`.
pub fn ids(steps: &[Step]) -> Vec<String> {
    steps.iter().map(|s| s.item.id.clone()).collect()
}

/// Calls `previous` `n` times starting from `cursor`, returning every step.
pub fn walk_back(rule: &Rule, items: &[ItemKey], cursor: &Cursor, n: usize) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::with_capacity(n);
    let mut cursor = cursor.clone();
    for _ in 0..n {
        let step = previous(rule, items, &cursor).expect("previous gives a step");
        cursor = step.cursor.clone();
        steps.push(step);
    }
    steps
}

/// The group an item belongs to under `by`, as a comparable string.
pub fn group_of(item: &ItemKey, by: GroupBy) -> String {
    match by {
        GroupBy::Season => format!("{:?}", item.season),
        GroupBy::Folder => item.folder().to_owned(),
    }
}
