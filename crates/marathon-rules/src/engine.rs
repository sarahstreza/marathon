//! Cursors and the `next` / `previous` entry points.

use serde::{Deserialize, Serialize};

use crate::Rule;
use crate::bag::Bag;
use crate::catalog::{self, Catalog};
use crate::grouped::{self, GroupedState};
use crate::item::ItemKey;

/// RNG domain for the `shuffled` rule's item order.
pub(crate) const DOMAIN_ITEMS: u64 = 1;
/// RNG domain for the order of groups in a `grouped` rule.
pub(crate) const DOMAIN_GROUPS: u64 = 2;
/// RNG domain for the order of items within a group of a `grouped` rule.
pub(crate) const DOMAIN_WITHIN: u64 = 3;

/// A channel's position in its rule's sequence.
///
/// Cursors are opaque to callers: store them (as JSON, via serde) and hand
/// them back. A cursor holds the seed for all of the rule's randomness, the
/// item it last returned, and whatever state its rule needs.
///
/// Passing a cursor made by a different rule (for example after the rule was
/// edited in config) is allowed: [`next`] continues from the cursor's item
/// as well as the new rule can, and [`previous`] may return `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    seed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<ItemKey>,
    #[serde(default)]
    state: State,
}

/// Rule-specific cursor state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum State {
    /// Nothing has played yet.
    #[default]
    Start,
    /// The `ordered` rule needs nothing beyond the current item.
    Ordered,
    /// The `shuffled` rule's current cycle.
    Shuffled { bag: Bag<String> },
    /// The `grouped` rule's state.
    Grouped(GroupedState),
}

impl Cursor {
    /// A cursor at the start of a sequence, whose shuffles derive from
    /// `seed`.
    ///
    /// This crate never picks seeds itself; the caller supplies one (for
    /// example from its own RNG when a channel is first created) so that
    /// rules stay pure and reproducible.
    pub fn seeded(seed: u64) -> Self {
        Self {
            seed,
            current: None,
            state: State::Start,
        }
    }

    /// The seed all of this cursor's shuffles derive from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The item this cursor is positioned at, as it was when the cursor was
    /// made. `None` for a cursor at the start.
    pub fn current(&self) -> Option<&ItemKey> {
        self.current.as_ref()
    }
}

impl Default for Cursor {
    /// Equivalent to `Cursor::seeded(0)`.
    fn default() -> Self {
        Self::seeded(0)
    }
}

/// The result of moving through a rule's sequence: the item to play and the
/// cursor to store for next time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub item: ItemKey,
    pub cursor: Cursor,
}

/// Returns the item after `cursor` under `rule`, or `None` if `items` is
/// empty.
///
/// With no cursor, returns the first item of the sequence, using seed 0;
/// pass [`Cursor::seeded`] to choose the seed. Items may change between calls
/// (library rescans): an item that has gone is skipped, and new items are
/// picked up no later than the next cycle.
pub fn next(rule: &Rule, items: &[ItemKey], cursor: Option<&Cursor>) -> Option<Step> {
    let start = Cursor::default();
    let cursor = cursor.unwrap_or(&start);
    let catalog = Catalog::new(items);
    let seed = cursor.seed;
    let from = cursor.current.as_ref();

    let (item, state) = match rule {
        Rule::Ordered => {
            let item = from
                .and_then(|f| catalog::after(catalog.items(), f))
                .or_else(|| catalog.items().first().copied())?;
            (item, State::Ordered)
        }
        Rule::Shuffled => {
            let present = |id: &String| catalog.contains(id);
            let advance = |bag: &Bag<String>| {
                bag.forward(present)
                    .or_else(|| bag.next_cycle(seed, DOMAIN_ITEMS, &catalog.ids()))
            };
            let bag = match (&cursor.state, from) {
                (State::Shuffled { bag }, _) => advance(bag),
                // Switched from another rule: deal a fresh cycle and treat
                // the current item as having just played in it.
                (_, Some(f)) if catalog.contains(&f.id) => {
                    Bag::generate(seed, DOMAIN_ITEMS, 0, &catalog.ids(), None, None)
                        .and_then(|b| b.positioned_at(&f.id))
                        .and_then(|b| advance(&b))
                }
                _ => {
                    let avoid = from.map(|f| &f.id);
                    Bag::generate(seed, DOMAIN_ITEMS, 0, &catalog.ids(), avoid, None)
                }
            }?;
            (catalog.get(bag.current()?)?, State::Shuffled { bag })
        }
        Rule::Grouped { by, groups, within } => {
            grouped::next(*by, *groups, *within, seed, &catalog, &cursor.state, from)?
        }
    };
    Some(Step {
        item: item.clone(),
        cursor: Cursor {
            seed,
            current: Some(item.clone()),
            state,
        },
    })
}

/// Returns the item before `cursor` under `rule`.
///
/// For a cursor returned by [`next`], this is the item `next` was called
/// from, and calling `next` on the result returns to where you were (as long
/// as the items have not changed). Returns `None` if there is nothing to go
/// back to:
///
/// - `items` is empty or the cursor is at the start;
/// - for shuffles, stepping back past the start of the previous cycle (only
///   one cycle of history is kept);
/// - the cursor was made by a different rule, unless the rule is `ordered`.
///
/// The `ordered` rule wraps around, so `previous` from its first item is its
/// last.
pub fn previous(rule: &Rule, items: &[ItemKey], cursor: &Cursor) -> Option<Step> {
    let catalog = Catalog::new(items);
    let from = cursor.current.as_ref()?;
    let seed = cursor.seed;

    let (item, state) = match rule {
        Rule::Ordered => {
            let item = catalog::before(catalog.items(), from)
                .or_else(|| catalog.items().last().copied())?;
            (item, State::Ordered)
        }
        Rule::Shuffled => {
            let State::Shuffled { bag } = &cursor.state else {
                return None;
            };
            let present = |id: &String| catalog.contains(id);
            let bag = bag.backward(present).or_else(|| bag.prior_cycle(present))?;
            (catalog.get(bag.current()?)?, State::Shuffled { bag })
        }
        Rule::Grouped { by, groups, within } => {
            grouped::previous(*by, *groups, *within, &catalog, &cursor.state, from)?
        }
    };
    Some(Step {
        item: item.clone(),
        cursor: Cursor {
            seed,
            current: Some(item.clone()),
            state,
        },
    })
}
