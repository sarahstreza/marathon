//! Pure programming-rules engine for Marathon channels.
//!
//! A rule decides which item a channel plays next. The entry points are
//! [`next`] (or [`next_weighted`]) and [`previous`], which take a [`Rule`],
//! the channel's items as [`ItemKey`]s, and a [`Cursor`], and return a
//! [`Step`]: the item to play and the cursor to store.
//!
//! An "item" here is anything the caller wants a rule to choose between.
//! The server also uses it for entries that stand for whole child channels
//! (PLAN.md M11); this crate neither knows nor cares.
//!
//! This crate must stay pure: no I/O, no clocks, no global RNG, and no
//! dependencies on other Marathon crates. Randomness comes from a seed stored
//! in the cursor (see [`Cursor::seeded`]), so the same inputs always give the
//! same sequence.

mod bag;
mod catalog;
mod engine;
mod grouped;
mod item;
mod natural;
mod rng;
mod weighted;

pub use engine::{Cursor, Step, next, next_weighted, previous};
pub use item::ItemKey;

use serde::{Deserialize, Serialize};

/// A channel's programming rule, as written in config and stored in the
/// database as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Rule {
    /// Natural order (season/episode when parsed, else natural sort of path).
    /// Loops at the end.
    Ordered,
    /// The order the channel's definition lists its items in (see
    /// [`ItemKey::position`]), with items that share a position, such as
    /// the contents of one folder, in natural order. Loops at the end.
    Listed,
    /// Bag shuffle: every item plays once per cycle before any repeats.
    Shuffled,
    /// Partition items into groups, order the groups, then order items within
    /// each group.
    Grouped {
        /// How items are partitioned into groups.
        by: GroupBy,
        /// Order in which groups play.
        groups: Order,
        /// Order in which items play within a group.
        within: Order,
    },
    /// Pick items at random in proportion to their weights, never the same
    /// item twice in a row (when there are two or more). Weights are given
    /// by the caller (see [`next_weighted`]); `by` records how.
    Weighted {
        /// How the caller works out each item's weight.
        #[serde(default)]
        by: WeightBy,
    },
}

/// How the weights of a [`Rule::Weighted`] rule are worked out. The rules
/// engine does not compute weights itself; this tells the caller what to
/// pass to [`next_weighted`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeightBy {
    /// Weights written in config, defaulting to 1.
    #[default]
    Manual,
    /// The number of items an entry stands for (1 for a plain item).
    ItemCount,
    /// The total duration of what an entry stands for.
    Duration,
}

/// How a [`Rule::Grouped`] rule partitions items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    /// Parsed season number.
    Season,
    /// Containing folder.
    Folder,
}

/// Ordering used for groups or for items within a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Order {
    /// Natural order.
    Ordered,
    /// Bag shuffle.
    Shuffled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Wrapper {
        rule: Rule,
    }

    fn parse(s: &str) -> Result<Rule, toml::de::Error> {
        toml::from_str::<Wrapper>(s).map(|w| w.rule)
    }

    #[test]
    fn parses_simple_rules() {
        assert_eq!(
            parse(r#"rule = { kind = "ordered" }"#).unwrap(),
            Rule::Ordered
        );
        assert_eq!(
            parse(r#"rule = { kind = "shuffled" }"#).unwrap(),
            Rule::Shuffled
        );
        assert_eq!(
            parse(r#"rule = { kind = "listed" }"#).unwrap(),
            Rule::Listed
        );
    }

    #[test]
    fn parses_grouped_rule() {
        let rule = parse(
            r#"rule = { kind = "grouped", by = "season", groups = "shuffled", within = "ordered" }"#,
        )
        .unwrap();
        assert_eq!(
            rule,
            Rule::Grouped {
                by: GroupBy::Season,
                groups: Order::Shuffled,
                within: Order::Ordered,
            }
        );
    }

    #[test]
    fn parses_weighted_rule() {
        assert_eq!(
            parse(r#"rule = { kind = "weighted", by = "item_count" }"#).unwrap(),
            Rule::Weighted {
                by: WeightBy::ItemCount
            }
        );
        assert_eq!(
            parse(r#"rule = { kind = "weighted" }"#).unwrap(),
            Rule::Weighted {
                by: WeightBy::Manual
            }
        );
        assert!(parse(r#"rule = { kind = "weighted", by = "mood" }"#).is_err());
    }

    #[test]
    fn rejects_unknown_kind_and_fields() {
        assert!(parse(r#"rule = { kind = "random" }"#).is_err());
        assert!(
            parse(r#"rule = { kind = "grouped", by = "season", groups = "ordered" }"#).is_err()
        );
        assert!(
            parse(
                r#"rule = { kind = "grouped", by = "season", groups = "ordered", within = "ordered", extra = 1 }"#
            )
            .is_err()
        );
    }
}
