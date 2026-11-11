//! Tests for the `weighted` rule (PLAN.md M11).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;

use common::{channel_entry, clip, ids};
use marathon_rules::{Cursor, ItemKey, Rule, Step, WeightBy, next, next_weighted, previous};
use proptest::prelude::*;
use proptest::sample::Index;

const WEIGHTED: Rule = Rule::Weighted {
    by: WeightBy::Manual,
};

/// `n` entries: alternately child channels and plain clips.
fn entries(n: usize) -> Vec<ItemKey> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                channel_entry(&format!("c{i}"))
            } else {
                clip(&format!("i{i}"), &format!("Misc/{i}.mkv"))
            }
        })
        .collect()
}

/// A weight function over `entries` from a parallel list of weights.
fn weigh(entries: &[ItemKey], weights: &[u64]) -> impl Fn(&ItemKey) -> u64 + use<> {
    let map: BTreeMap<String, u64> = entries
        .iter()
        .map(|e| e.id.clone())
        .zip(weights.iter().copied())
        .collect();
    move |e: &ItemKey| map.get(&e.id).copied().unwrap_or(0)
}

/// Calls `next_weighted` `n` times from `cursor`.
fn walk(
    items: &[ItemKey],
    weight: &dyn Fn(&ItemKey) -> u64,
    cursor: Option<Cursor>,
    n: usize,
) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::with_capacity(n);
    let mut cursor = cursor;
    for _ in 0..n {
        let step = next_weighted(&WEIGHTED, items, cursor.as_ref(), weight).unwrap();
        cursor = Some(step.cursor.clone());
        out.push(step);
    }
    out
}

/// Weights for 3–7 entries in which no entry has more than half the total
/// (the condition under which exact shares without repeats are possible).
fn balanced_weights() -> impl Strategy<Value = Vec<u64>> {
    prop::collection::vec(1u64..10, 3..=7).prop_filter("no entry over half", |w| {
        let total: u64 = w.iter().sum();
        w.iter().all(|x| 2 * x <= total)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn frequencies_follow_the_weights(weights in balanced_weights(), seed: u64) {
        let items = entries(weights.len());
        let weight = weigh(&items, &weights);
        let picks = 12_000;
        let seq = walk(&items, &weight, Some(Cursor::seeded(seed)), picks);
        let total: u64 = weights.iter().sum();
        for (item, w) in items.iter().zip(&weights) {
            let count = seq.iter().filter(|s| s.item.id == item.id).count();
            let got = count as f64 / picks as f64;
            let want = *w as f64 / total as f64;
            prop_assert!(
                (got - want).abs() < 0.025,
                "{}: picked {:.3} of the time, weight share {:.3} ({:?})",
                item.id, got, want, weights
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn never_repeats_with_two_or_more_entries(
        weights in prop::collection::vec(0u64..20, 2..12),
        seed: u64,
    ) {
        let items = entries(weights.len());
        let weight = weigh(&items, &weights);
        let positive = weights.iter().filter(|w| **w > 0).count();
        let seq = ids(&walk(&items, &weight, Some(Cursor::seeded(seed)), 200));
        if positive >= 2 || positive == 0 {
            for pair in seq.windows(2) {
                prop_assert_ne!(&pair[0], &pair[1]);
            }
        }
        // Zero-weight entries never play while anything else can.
        if positive > 0 {
            for id in &seq {
                let i = items.iter().position(|e| &e.id == id).unwrap();
                prop_assert!(weights[i] > 0, "{} has weight 0", id);
            }
        }
    }

    #[test]
    fn same_seed_gives_the_same_sequence(
        weights in prop::collection::vec(1u64..20, 1..12),
        seed: u64,
    ) {
        let items = entries(weights.len());
        let weight = weigh(&items, &weights);
        let a = walk(&items, &weight, Some(Cursor::seeded(seed)), 100);
        let b = walk(&items, &weight, Some(Cursor::seeded(seed)), 100);
        prop_assert_eq!(&a, &b);
        // Listing the entries in another order changes nothing.
        let mut reversed = items.clone();
        reversed.reverse();
        let c = walk(&reversed, &weight, Some(Cursor::seeded(seed)), 100);
        prop_assert_eq!(ids(&a), ids(&c));
    }

    #[test]
    fn cursor_round_trips_through_json(
        weights in prop::collection::vec(1u64..20, 1..12),
        seed: u64,
        split in any::<Index>(),
    ) {
        let items = entries(weights.len());
        let weight = weigh(&items, &weights);
        let straight = walk(&items, &weight, Some(Cursor::seeded(seed)), 80);
        let k = split.index(straight.len());
        let json = serde_json::to_string(&straight[k].cursor).unwrap();
        let restored: Cursor = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&restored, &straight[k].cursor);
        let resumed = walk(&items, &weight, Some(restored), 80 - k - 1);
        prop_assert_eq!(ids(&resumed), ids(&straight[k + 1..]));
    }

    #[test]
    fn previous_after_next_returns_the_prior_entry(
        weights in prop::collection::vec(1u64..20, 1..12),
        seed: u64,
        at in any::<Index>(),
    ) {
        let items = entries(weights.len());
        let weight = weigh(&items, &weights);
        let steps = walk(&items, &weight, Some(Cursor::seeded(seed)), 40);
        let k = 1 + at.index(steps.len() - 1);
        let back = previous(&WEIGHTED, &items, &steps[k].cursor).expect("one step back");
        prop_assert_eq!(&back.item, &steps[k - 1].item);
        let again = next_weighted(&WEIGHTED, &items, Some(&back.cursor), &weight).unwrap();
        prop_assert_eq!(&again.item, &steps[k].item);
    }

    #[test]
    fn removed_entries_are_never_picked(
        weights in prop::collection::vec(1u64..20, 2..12),
        seed: u64,
        at in any::<Index>(),
    ) {
        let items = entries(weights.len());
        let weight = weigh(&items, &weights);
        let steps = walk(&items, &weight, Some(Cursor::seeded(seed)), 30);
        let cursor = &steps[at.index(steps.len())].cursor;
        let current = cursor.current().unwrap().id.clone();
        let remaining: Vec<ItemKey> = items.iter().filter(|i| i.id != current).cloned().collect();
        let after = walk(&remaining, &weight, Some(cursor.clone()), 30);
        prop_assert!(after.iter().all(|s| remaining.contains(&s.item)));
    }
}

#[test]
fn edge_cases() {
    assert!(next(&WEIGHTED, &[], None).is_none());
    let one = vec![channel_entry("only")];
    let seq = ids(&walk(&one, &|_| 3, None, 5));
    assert!(seq.iter().all(|id| id == "channel:only"));
    // Previous from the start has nothing to go back to.
    let first = next(&WEIGHTED, &one, None).unwrap();
    assert!(previous(&WEIGHTED, &one, &first.cursor).is_none());
}

#[test]
fn switching_to_weighted_from_another_rule_keeps_going() {
    let items = entries(5);
    let ordered = common::walk(&Rule::Ordered, &items, None, 3);
    let cursor = ordered.last().unwrap().cursor.clone();
    let step = next(&WEIGHTED, &items, Some(&cursor)).unwrap();
    assert_ne!(step.item, ordered.last().unwrap().item);
    // And back again.
    let back = next(&Rule::Shuffled, &items, Some(&step.cursor)).unwrap();
    assert!(items.contains(&back.item));
}

#[test]
fn other_rules_ignore_weights() {
    let items = entries(6);
    let plain = common::walk(&Rule::Ordered, &items, None, 12);
    let weighted = walk_rule(&Rule::Ordered, &items, &|_| 0, 12);
    assert_eq!(ids(&plain), ids(&weighted));
}

fn walk_rule(
    rule: &Rule,
    items: &[ItemKey],
    weight: &dyn Fn(&ItemKey) -> u64,
    n: usize,
) -> Vec<Step> {
    let mut cursor: Option<Cursor> = None;
    (0..n)
        .map(|_| {
            let step = next_weighted(rule, items, cursor.as_ref(), weight).unwrap();
            cursor = Some(step.cursor.clone());
            step
        })
        .collect()
}
