//! Property tests for the rules engine (PLAN.md §6 and milestone M1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{all_rules, group_of, ids, walk};
use marathon_rules::{Cursor, GroupBy, ItemKey, Order, Rule, next, previous};
use proptest::prelude::*;
use proptest::sample::Index;

/// Folder names chosen to exercise natural sorting and nesting.
const FOLDERS: &[&str] = &["", "A", "B", "A/Season 2", "A/Season 10", "Essays"];

/// One random entry, made unique by `n`: usually a video item, sometimes
/// an entry standing for a whole child channel, keyed the way the server
/// keys them (PLAN.md M11). The rules must not be able to tell the
/// difference, so every property below holds for mixes of both.
fn item(n: usize) -> impl Strategy<Value = ItemKey> {
    prop_oneof![
        4 => video(n),
        1 => Just(common::channel_entry(&format!("child-{n}"))),
    ]
}

/// One random video item; its ID and path are made unique by `n`.
fn video(n: usize) -> impl Strategy<Value = ItemKey> {
    (
        0..FOLDERS.len(),
        prop::option::of(0u32..4),
        prop::option::of(0u32..6),
        any::<bool>(),
    )
        .prop_map(move |(folder, season, episode, has_show)| {
            let folder = FOLDERS[folder];
            let sep = if folder.is_empty() { "" } else { "/" };
            ItemKey {
                id: format!("id{n:03}"),
                path: format!("{folder}{sep}clip {n}.mkv"),
                show: has_show.then(|| "Show".to_owned()),
                season,
                episode,
            }
        })
}

/// A random set of up to `max` items with unique IDs.
fn items(max: usize) -> impl Strategy<Value = Vec<ItemKey>> {
    (0..=max).prop_flat_map(|n| (0..n).map(item).collect::<Vec<_>>())
}

fn rule() -> impl Strategy<Value = Rule> {
    prop::sample::select(all_rules())
}

fn grouped_rule() -> impl Strategy<Value = Rule> {
    prop::sample::select(
        all_rules()
            .into_iter()
            .filter(|r| matches!(r, Rule::Grouped { .. }))
            .collect::<Vec<_>>(),
    )
}

fn sorted_ids(items: &[ItemKey]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    v.sort();
    v
}

/// Steps enough to complete several cycles of any rule over `n` items.
fn several_cycles(n: usize) -> usize {
    3 * n + 1
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn ordered_is_natural_order_repeated(items in items(40)) {
        prop_assume!(!items.is_empty());
        let mut sorted = items.clone();
        sorted.sort_by(ItemKey::natural_order);
        let expected: Vec<String> = sorted.iter().map(|i| i.id.clone()).collect();
        let seq = ids(&walk(&Rule::Ordered, &items, None, 3 * items.len()));
        for cycle in seq.chunks(items.len()) {
            prop_assert_eq!(cycle, expected.as_slice());
        }
    }

    #[test]
    fn shuffled_plays_each_item_once_per_cycle(items in items(200), seed: u64) {
        let n = items.len();
        if n == 0 {
            prop_assert!(next(&Rule::Shuffled, &items, Some(&Cursor::seeded(seed))).is_none());
            return Ok(());
        }
        let seq = ids(&walk(&Rule::Shuffled, &items, Some(Cursor::seeded(seed)), 3 * n));
        let cycles: Vec<&[String]> = seq.chunks(n).collect();
        for cycle in &cycles {
            let mut sorted = cycle.to_vec();
            sorted.sort();
            prop_assert_eq!(sorted, sorted_ids(&items));
        }
        if n >= 2 {
            for pair in cycles.windows(2) {
                prop_assert_ne!(pair[0].last(), pair[1].first());
            }
        }
    }

    #[test]
    fn grouped_plays_whole_groups(items in items(60), rule in grouped_rule(), seed: u64) {
        prop_assume!(!items.is_empty());
        let Rule::Grouped { by, groups, within } = rule else { unreachable!() };

        let mut members: BTreeMap<String, Vec<ItemKey>> = BTreeMap::new();
        for item in &items {
            members.entry(group_of(item, by)).or_default().push(item.clone());
        }
        for m in members.values_mut() {
            m.sort_by(ItemKey::natural_order);
        }

        // Cut the sequence into visits: each starts at a group and must be
        // exactly that group's items (so groups never interleave).
        let seq = walk(&rule, &items, Some(Cursor::seeded(seed)), 3 * items.len());
        let mut visits: Vec<String> = Vec::new();
        let mut rest = seq.as_slice();
        while !rest.is_empty() {
            let group = group_of(&rest[0].item, by);
            let expected = &members[&group];
            let take = expected.len().min(rest.len());
            let (visit, tail) = rest.split_at(take);
            rest = tail;
            if visit.len() < expected.len() {
                break; // The walk ended mid-visit.
            }
            let got: Vec<&ItemKey> = visit.iter().map(|s| &s.item).collect();
            match within {
                Order::Ordered => {
                    let want: Vec<&ItemKey> = expected.iter().collect();
                    prop_assert_eq!(got, want, "group {} out of order", group);
                }
                Order::Shuffled => {
                    let mut got: Vec<String> = got.iter().map(|i| i.id.clone()).collect();
                    got.sort();
                    prop_assert_eq!(got, sorted_ids(expected), "group {} incomplete", group);
                }
            }
            visits.push(group);
        }

        // Every group plays once per cycle of groups.
        let all_groups: Vec<String> = members.keys().cloned().collect();
        let cycles: Vec<&[String]> = visits.chunks(all_groups.len()).collect();
        let complete = cycles.iter().filter(|c| c.len() == all_groups.len());
        for cycle in complete.clone() {
            let mut sorted = cycle.to_vec();
            sorted.sort();
            prop_assert_eq!(&sorted, &all_groups);
        }
        // Ordered groups repeat the same order every cycle.
        if groups == Order::Ordered {
            let first = cycles[0];
            for cycle in complete {
                prop_assert_eq!(*cycle, first);
            }
        }
    }

    #[test]
    fn same_seed_and_items_give_the_same_sequence(
        items in items(50),
        rule in rule(),
        seed: u64,
        reorder: Vec<Index>,
    ) {
        prop_assume!(!items.is_empty());
        let steps = several_cycles(items.len());
        let a = walk(&rule, &items, Some(Cursor::seeded(seed)), steps);
        let b = walk(&rule, &items, Some(Cursor::seeded(seed)), steps);
        prop_assert_eq!(&a, &b);

        // A rescan that lists the same items in another order changes nothing.
        let mut shuffled = items.clone();
        for (i, idx) in reorder.iter().enumerate() {
            let len = shuffled.len();
            shuffled.swap(i % len, idx.index(len));
        }
        let c = walk(&rule, &shuffled, Some(Cursor::seeded(seed)), steps);
        prop_assert_eq!(ids(&a), ids(&c));
    }

    #[test]
    fn cursor_json_round_trip_continues_identically(
        items in items(40),
        rule in rule(),
        seed: u64,
        split in any::<Index>(),
    ) {
        prop_assume!(!items.is_empty());
        let steps = several_cycles(items.len());
        let straight = walk(&rule, &items, Some(Cursor::seeded(seed)), steps);

        let k = split.index(steps);
        let json = serde_json::to_string(&straight[k].cursor).unwrap();
        let restored: Cursor = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&restored, &straight[k].cursor);
        let resumed = walk(&rule, &items, Some(restored), steps - k - 1);
        prop_assert_eq!(ids(&resumed), ids(&straight[k + 1..]));
    }

    #[test]
    fn previous_after_next_returns_the_prior_item(
        items in items(40),
        rule in rule(),
        seed: u64,
        at in any::<Index>(),
    ) {
        prop_assume!(!items.is_empty());
        let steps = walk(&rule, &items, Some(Cursor::seeded(seed)), several_cycles(items.len()));
        let k = 1 + at.index(steps.len() - 1);
        let back = previous(&rule, &items, &steps[k].cursor).expect("one step back");
        prop_assert_eq!(&back.item, &steps[k - 1].item);
        // And `next` from there returns to where we were.
        let again = next(&rule, &items, Some(&back.cursor)).unwrap();
        prop_assert_eq!(&again.item, &steps[k].item);
    }

    #[test]
    fn removing_the_current_item_advances_to_a_valid_one(
        items in items(40),
        rule in rule(),
        seed: u64,
        at in any::<Index>(),
    ) {
        prop_assume!(!items.is_empty());
        let steps = walk(&rule, &items, Some(Cursor::seeded(seed)), several_cycles(items.len()));
        let cursor = &steps[at.index(steps.len())].cursor;
        let current = cursor.current().unwrap().id.clone();
        let remaining: Vec<ItemKey> = items.iter().filter(|i| i.id != current).cloned().collect();

        match next(&rule, &remaining, Some(cursor)) {
            None => prop_assert!(remaining.is_empty()),
            Some(step) => {
                prop_assert!(remaining.contains(&step.item));
                // And the rule keeps going from there.
                let more = walk(&rule, &remaining, Some(step.cursor), 2 * remaining.len());
                prop_assert!(more.iter().all(|s| remaining.contains(&s.item)));
            }
        }
        if let Some(step) = previous(&rule, &remaining, cursor) {
            prop_assert!(remaining.contains(&step.item));
        }
    }

    #[test]
    fn added_items_play_no_later_than_the_next_cycle(
        items in items(40),
        rule in rule(),
        seed: u64,
        at in any::<Index>(),
        new in item(999),
    ) {
        prop_assume!(!items.is_empty());
        let steps = walk(&rule, &items, Some(Cursor::seeded(seed)), several_cycles(items.len()));
        let cursor = steps[at.index(steps.len())].cursor.clone();

        let mut rescanned = items.clone();
        rescanned.push(new.clone());
        // At most the rest of this cycle, then one full new cycle. For
        // grouped rules a "cycle" is one pass over every group, which is
        // also bounded by the item count.
        let bound = 2 * rescanned.len();
        let after = walk(&rule, &rescanned, Some(cursor), bound);
        prop_assert!(
            after.iter().any(|s| s.item.id == new.id),
            "new item not played within {} steps", bound
        );
    }

    #[test]
    fn switching_rules_never_panics(
        items in items(30),
        from_rule in rule(),
        to_rule in rule(),
        seed: u64,
        at in any::<Index>(),
    ) {
        prop_assume!(!items.is_empty());
        let steps = walk(&from_rule, &items, Some(Cursor::seeded(seed)), several_cycles(items.len()));
        let cursor = steps[at.index(steps.len())].cursor.clone();
        let after = walk(&to_rule, &items, Some(cursor.clone()), several_cycles(items.len()));
        prop_assert!(after.iter().all(|s| items.contains(&s.item)));
        if let Some(step) = previous(&to_rule, &items, &cursor) {
            prop_assert!(items.contains(&step.item));
        }
    }
}

#[test]
fn group_of_matches_engine_grouping() {
    // Sanity check on the test helper itself, against a hand-built case.
    let rule = Rule::Grouped {
        by: GroupBy::Folder,
        groups: Order::Ordered,
        within: Order::Ordered,
    };
    let items = vec![
        common::clip("x", "A/Season 10/x.mkv"),
        common::clip("y", "A/Season 2/y.mkv"),
    ];
    let seq = walk(&rule, &items, None, 2);
    let groups: BTreeSet<String> = seq
        .iter()
        .map(|s| group_of(&s.item, GroupBy::Folder))
        .collect();
    assert_eq!(groups.len(), 2);
    assert_eq!(ids(&seq), ["y", "x"]);
}
