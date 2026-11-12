//! Example-based tests of the rules engine through its public API.
//!
//! Property tests over random item sets live in `properties.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{all_rules, at, clip, episode, ids, show, walk, walk_back};
use marathon_rules::{Cursor, GroupBy, ItemKey, Order, Rule, next, previous};

#[test]
fn ordered_walks_season_episode_order_then_loops() {
    // Listed out of order, with episode 10 to catch string sorting.
    let mut items = vec![
        episode(2, 1),
        episode(1, 10),
        episode(1, 2),
        episode(1, 1),
        episode(2, 2),
    ];
    items.reverse();
    let seq = ids(&walk(&Rule::Ordered, &items, None, 7));
    assert_eq!(
        seq,
        ["s1e1", "s1e2", "s1e10", "s2e1", "s2e2", "s1e1", "s1e2"]
    );
}

#[test]
fn ordered_falls_back_to_natural_path_order() {
    let items = vec![
        clip("ten", "Essays/Ep 10 - Ten.mkv"),
        clip("two", "Essays/Ep 2 - Two.mkv"),
        clip("one", "Essays/Ep 1 - One.mkv"),
    ];
    let seq = ids(&walk(&Rule::Ordered, &items, None, 4));
    assert_eq!(seq, ["one", "two", "ten", "one"]);
}

#[test]
fn ordered_previous_wraps_to_the_last_item() {
    let items = show(1, 3);
    let first = next(&Rule::Ordered, &items, None).unwrap();
    let back = previous(&Rule::Ordered, &items, &first.cursor).unwrap();
    assert_eq!(back.item.id, "s1e3");
}

#[test]
fn ordered_slots_new_items_into_place_immediately() {
    let items = show(2, 2);
    let at_s1e2 = walk(&Rule::Ordered, &items, None, 2).pop().unwrap();
    assert_eq!(at_s1e2.item.id, "s1e2");

    let mut rescanned = items.clone();
    rescanned.push(episode(1, 3));
    let step = next(&Rule::Ordered, &rescanned, Some(&at_s1e2.cursor)).unwrap();
    assert_eq!(step.item.id, "s1e3");
}

#[test]
fn ordered_continues_after_a_removed_current_item() {
    let items = show(1, 4);
    let at_e2 = walk(&Rule::Ordered, &items, None, 2).pop().unwrap();
    let without_e2: Vec<ItemKey> = items.into_iter().filter(|i| i.id != "s1e2").collect();
    let step = next(&Rule::Ordered, &without_e2, Some(&at_e2.cursor)).unwrap();
    assert_eq!(step.item.id, "s1e3");
    let back = previous(&Rule::Ordered, &without_e2, &at_e2.cursor).unwrap();
    assert_eq!(back.item.id, "s1e1");
}

/// A hand-picked running order: a two-parter from season 2, then a special,
/// then a whole folder of essays (sharing one position), then season 1's
/// opener. Season 1 episode 2 is not listed at all (excluded).
fn running_order() -> Vec<ItemKey> {
    vec![
        at(3, episode(1, 1)),
        at(2, clip("ess10", "Essays/Ep 10.mkv")),
        at(0, episode(2, 1)),
        at(2, clip("ess2", "Essays/Ep 2.mkv")),
        at(1, clip("special", "Show/Specials/Christmas.mkv")),
        at(0, episode(2, 2)),
    ]
}

#[test]
fn listed_walks_list_positions_then_loops() {
    let seq = ids(&walk(&Rule::Listed, &running_order(), None, 8));
    assert_eq!(
        seq,
        [
            "s2e1", "s2e2", "special", "ess2", "ess10", "s1e1", "s2e1", "s2e2"
        ]
    );
}

#[test]
fn listed_previous_wraps_to_the_last_item() {
    let items = running_order();
    let first = next(&Rule::Listed, &items, None).unwrap();
    let back = previous(&Rule::Listed, &items, &first.cursor).unwrap();
    assert_eq!(back.item.id, "s1e1");
}

#[test]
fn listed_continues_after_a_removed_current_item() {
    let items = running_order();
    let at_special = walk(&Rule::Listed, &items, None, 3).pop().unwrap();
    assert_eq!(at_special.item.id, "special");
    let without: Vec<ItemKey> = items.into_iter().filter(|i| i.id != "special").collect();
    let step = next(&Rule::Listed, &without, Some(&at_special.cursor)).unwrap();
    assert_eq!(step.item.id, "ess2");
    let back = previous(&Rule::Listed, &without, &at_special.cursor).unwrap();
    assert_eq!(back.item.id, "s2e2");
}

#[test]
fn listed_follows_an_item_moved_to_a_new_place() {
    let items = running_order();
    let at_special = walk(&Rule::Listed, &items, None, 3).pop().unwrap();
    // The config is edited to move the special to the very end.
    let reordered: Vec<ItemKey> = items
        .into_iter()
        .map(|i| if i.id == "special" { at(9, i) } else { i })
        .collect();
    let step = next(&Rule::Listed, &reordered, Some(&at_special.cursor)).unwrap();
    assert_eq!(step.item.id, "s2e1", "wraps around from the new last place");
}

#[test]
fn listed_puts_unlisted_items_last_in_natural_order() {
    let items = vec![episode(1, 2), at(0, episode(2, 1)), episode(1, 1)];
    let seq = ids(&walk(&Rule::Listed, &items, None, 3));
    assert_eq!(seq, ["s2e1", "s1e1", "s1e2"]);
}

#[test]
fn ordered_ignores_list_positions() {
    let seq = ids(&walk(&Rule::Ordered, &running_order(), None, 6));
    assert_eq!(
        seq,
        ["ess2", "ess10", "special", "s1e1", "s2e1", "s2e2"],
        "unparsed items (no show) first, then by season and episode"
    );
}

#[test]
fn taskmaster_rule_plays_whole_seasons_in_shuffled_order() {
    let rule = Rule::Grouped {
        by: GroupBy::Season,
        groups: Order::Shuffled,
        within: Order::Ordered,
    };
    let items = show(5, 3);
    let seq = walk(&rule, &items, Some(Cursor::seeded(1234)), 30);
    let seasons: Vec<u32> = seq.iter().map(|s| s.item.season.unwrap()).collect();
    for (visit, chunk) in seq.chunks(3).enumerate() {
        let season = chunk[0].item.season.unwrap();
        let expected: Vec<String> = (1..=3).map(|e| format!("s{season}e{e}")).collect();
        assert_eq!(
            ids(chunk),
            expected,
            "visit {visit} is a whole season, in order"
        );
    }
    for cycle in seasons.chunks(15) {
        let mut visited: Vec<u32> = cycle.iter().step_by(3).copied().collect();
        visited.sort_unstable();
        assert_eq!(visited, [1, 2, 3, 4, 5]);
    }
}

#[test]
fn grouped_by_folder_with_ordered_groups() {
    let rule = Rule::Grouped {
        by: GroupBy::Folder,
        groups: Order::Ordered,
        within: Order::Ordered,
    };
    let items = vec![
        clip("b2", "Level Design/b 2.mkv"),
        clip("a10", "Game Design/a 10.mkv"),
        clip("b1", "Level Design/b 1.mkv"),
        clip("a2", "Game Design/a 2.mkv"),
    ];
    let seq = ids(&walk(&rule, &items, None, 5));
    assert_eq!(seq, ["a2", "a10", "b1", "b2", "a2"]);
}

#[test]
fn season_groups_put_unparsed_items_last() {
    let rule = Rule::Grouped {
        by: GroupBy::Season,
        groups: Order::Ordered,
        within: Order::Ordered,
    };
    let items = vec![
        clip("extra", "Show/Extras/x.mkv"),
        episode(2, 1),
        episode(1, 1),
    ];
    let seq = ids(&walk(&rule, &items, None, 4));
    assert_eq!(seq, ["s1e1", "s2e1", "extra", "s1e1"]);
}

#[test]
fn every_rule_handles_zero_items() {
    for rule in all_rules() {
        assert!(next(&rule, &[], None).is_none(), "{rule:?}");
        assert!(
            next(&rule, &[], Some(&Cursor::seeded(5))).is_none(),
            "{rule:?}"
        );
        // A cursor from a non-empty channel whose items all went away.
        let items = show(2, 2);
        let cursor = walk(&rule, &items, None, 3).pop().unwrap().cursor;
        assert!(next(&rule, &[], Some(&cursor)).is_none(), "{rule:?}");
        assert!(previous(&rule, &[], &cursor).is_none(), "{rule:?}");
    }
}

#[test]
fn every_rule_repeats_a_single_item() {
    let items = vec![episode(1, 1)];
    for rule in all_rules() {
        let steps = walk(&rule, &items, None, 5);
        assert!(steps.iter().all(|s| s.item.id == "s1e1"), "{rule:?}");
        let back = walk_back(&rule, &items, &steps[4].cursor, 1);
        assert_eq!(back[0].item.id, "s1e1", "{rule:?}");
    }
}

#[test]
fn previous_from_the_start_is_none() {
    for rule in all_rules() {
        assert!(previous(&rule, &show(2, 2), &Cursor::seeded(3)).is_none());
    }
}

#[test]
fn shuffled_previous_reaches_back_one_cycle_only() {
    let items = show(1, 4);
    let steps = walk(&Rule::Shuffled, &items, Some(Cursor::seeded(9)), 9);
    // Step 8 (index 8) is the first of the third cycle. Going back reaches
    // the whole second cycle, but not the first.
    let back = walk_back(&Rule::Shuffled, &items, &steps[8].cursor, 4);
    let expected: Vec<String> = steps[4..8]
        .iter()
        .rev()
        .map(|s| s.item.id.clone())
        .collect();
    assert_eq!(ids(&back), expected);
    assert!(previous(&Rule::Shuffled, &items, &back[3].cursor).is_none());
}

#[test]
fn ordered_previous_reaches_back_indefinitely() {
    let items = show(2, 3);
    let steps = walk(&Rule::Ordered, &items, None, 3);
    let back = walk_back(&Rule::Ordered, &items, &steps[2].cursor, 8);
    assert_eq!(
        ids(&back),
        [
            "s1e2", "s1e1", "s2e3", "s2e2", "s2e1", "s1e3", "s1e2", "s1e1"
        ]
    );
}

#[test]
fn changing_the_rule_continues_from_the_current_item() {
    let items = show(3, 3);
    let shuffled = walk(&Rule::Shuffled, &items, Some(Cursor::seeded(4)), 2)
        .pop()
        .unwrap();
    let current = shuffled.item.clone();

    // Ordered picks up right after the item that was playing.
    let step = next(&Rule::Ordered, &items, Some(&shuffled.cursor)).unwrap();
    let mut sorted = items.clone();
    sorted.sort_by(ItemKey::natural_order);
    let pos = sorted.iter().position(|i| i.id == current.id).unwrap();
    assert_eq!(step.item, sorted[(pos + 1) % sorted.len()]);

    // Every other rule at least gives a valid item without panicking.
    for rule in all_rules() {
        let step = next(&rule, &items, Some(&shuffled.cursor)).unwrap();
        assert!(items.contains(&step.item), "{rule:?}");
    }
}

#[test]
fn ordered_to_grouped_ordered_keeps_its_place() {
    let items = show(3, 3);
    let at = walk(&Rule::Ordered, &items, None, 5).pop().unwrap();
    assert_eq!(at.item.id, "s2e2");
    let rule = Rule::Grouped {
        by: GroupBy::Season,
        groups: Order::Ordered,
        within: Order::Ordered,
    };
    let seq = ids(&walk(&rule, &items, Some(at.cursor), 3));
    assert_eq!(seq, ["s2e3", "s3e1", "s3e2"]);
}

#[test]
fn duplicate_ids_are_played_once() {
    let items = vec![episode(1, 1), episode(1, 2), episode(1, 1)];
    let seq = ids(&walk(&Rule::Shuffled, &items, None, 4));
    let mut first_cycle = seq[..2].to_vec();
    first_cycle.sort();
    assert_eq!(first_cycle, ["s1e1", "s1e2"]);
}

#[test]
fn cursor_json_is_stable_and_readable() {
    let items = show(1, 2);
    let step = next(&Rule::Ordered, &items, Some(&Cursor::seeded(7))).unwrap();
    let json = serde_json::to_value(&step.cursor).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "seed": 7,
            "current": {
                "id": "s1e1",
                "path": "Show/Season 1/S01E01.mkv",
                "show": "Show",
                "season": 1,
                "episode": 1
            },
            "state": { "kind": "ordered" }
        })
    );
    // A bare seed is enough to start from.
    let start: Cursor = serde_json::from_str(r#"{"seed": 7}"#).unwrap();
    assert_eq!(start, Cursor::seeded(7));
}

#[test]
fn garbage_state_does_not_panic() {
    let items = show(2, 3);
    let cursors = [
        r#"{"seed":1,"state":{"kind":"shuffled","bag":{"cycle":0,"order":[],"pos":99}}}"#,
        r#"{"seed":1,"state":{"kind":"shuffled","bag":{"cycle":0,"order":["nope"],"pos":0,"prior":["gone"]}}}"#,
        r#"{"seed":1,"state":{"kind":"grouped","group":{"folder":"x"}}}"#,
        r#"{"seed":1,"current":{"id":"zz","path":"zz"},"state":{"kind":"grouped","group":{"season":9},"groups":{"cycle":0,"order":[{"season":9}],"pos":5},"within":{"cycle":0,"order":[],"pos":0}}}"#,
    ];
    for json in cursors {
        let cursor: Cursor = serde_json::from_str(json).unwrap();
        for rule in all_rules() {
            let step = next(&rule, &items, Some(&cursor)).expect("items are non-empty");
            assert!(items.contains(&step.item), "{rule:?} {json}");
            let _ = previous(&rule, &items, &cursor);
        }
    }
}
