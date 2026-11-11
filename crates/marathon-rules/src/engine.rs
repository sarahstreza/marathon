//! Cursors and the `next` / `previous` entry points.

use serde::{Deserialize, Serialize};

use crate::Rule;
use crate::bag::Bag;
use crate::catalog::{self, Catalog};
use crate::grouped::{self, GroupedState};
use crate::item::ItemKey;
use crate::weighted::{self, WeightedState};

/// RNG domain for the `shuffled` rule's item order.
pub(crate) const DOMAIN_ITEMS: u64 = 1;
/// RNG domain for the order of groups in a `grouped` rule.
pub(crate) const DOMAIN_GROUPS: u64 = 2;
/// RNG domain for the order of items within a group of a `grouped` rule.
pub(crate) const DOMAIN_WITHIN: u64 = 3;
/// RNG domain for the `weighted` rule's picks.
pub(crate) const DOMAIN_WEIGHTED: u64 = 4;

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
    /// Nor does the `listed` rule.
    Listed,
    /// The `shuffled` rule's current cycle.
    Shuffled { bag: Bag<String> },
    /// The `grouped` rule's state.
    Grouped(GroupedState),
    /// The `weighted` rule's state.
    Weighted(WeightedState),
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
///
/// The [`Rule::Weighted`] rule weighs every item equally here; use
/// [`next_weighted`] to give it weights.
pub fn next(rule: &Rule, items: &[ItemKey], cursor: Option<&Cursor>) -> Option<Step> {
    next_weighted(rule, items, cursor, &|_| 1)
}

/// Like [`next`], with `weight` giving each item's weight for the
/// [`Rule::Weighted`] rule. Other rules ignore the weights.
///
/// Weights are relative: an item of weight 2 is picked twice as often as
/// one of weight 1, in the long run. Items of weight 0 are never picked,
/// unless every item has weight 0, in which case they all count equally.
/// The weighted rule never picks the current item again straight away when
/// there is any other item to pick.
pub fn next_weighted(
    rule: &Rule,
    items: &[ItemKey],
    cursor: Option<&Cursor>,
    weight: &dyn Fn(&ItemKey) -> u64,
) -> Option<Step> {
    let start = Cursor::default();
    let cursor = cursor.unwrap_or(&start);
    let catalog = catalog_for(rule, items);
    let seed = cursor.seed;
    let from = cursor.current.as_ref();

    let (item, state) = match rule {
        Rule::Ordered => {
            let item = from
                .and_then(|f| catalog::after(catalog.items(), f, ItemKey::natural_order))
                .or_else(|| catalog.items().first().copied())?;
            (item, State::Ordered)
        }
        Rule::Listed => {
            let item = from
                .map(|f| listed_from(&catalog, f))
                .and_then(|f| catalog::after(catalog.items(), f, ItemKey::listed_order))
                .or_else(|| catalog.items().first().copied())?;
            (item, State::Listed)
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
        Rule::Weighted { .. } => {
            let state = match &cursor.state {
                State::Weighted(state) => Some(state),
                _ => None,
            };
            let (item, state) = weighted::next(&catalog, weight, seed, state, from)?;
            (item, State::Weighted(state))
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
/// - for the weighted rule, stepping back more than 50 picks;
/// - the cursor was made by a different rule, unless the rule is `ordered`
///   or `listed`.
///
/// The `ordered` and `listed` rules wrap around, so `previous` from their
/// first item is their last.
pub fn previous(rule: &Rule, items: &[ItemKey], cursor: &Cursor) -> Option<Step> {
    let catalog = catalog_for(rule, items);
    let from = cursor.current.as_ref()?;
    let seed = cursor.seed;

    let (item, state) = match rule {
        Rule::Ordered => {
            let item = catalog::before(catalog.items(), from, ItemKey::natural_order)
                .or_else(|| catalog.items().last().copied())?;
            (item, State::Ordered)
        }
        Rule::Listed => {
            let from = listed_from(&catalog, from);
            let item = catalog::before(catalog.items(), from, ItemKey::listed_order)
                .or_else(|| catalog.items().last().copied())?;
            (item, State::Listed)
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
        Rule::Weighted { .. } => {
            let State::Weighted(state) = &cursor.state else {
                return None;
            };
            let (item, state) = weighted::previous(&catalog, state)?;
            (item, State::Weighted(state))
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

/// The items of one call, sorted the way `rule` walks them.
fn catalog_for<'a>(rule: &Rule, items: &'a [ItemKey]) -> Catalog<'a> {
    match rule {
        Rule::Listed => Catalog::sorted_by(items, ItemKey::listed_order),
        Rule::Ordered | Rule::Shuffled | Rule::Grouped { .. } | Rule::Weighted { .. } => {
            Catalog::new(items)
        }
    }
}

/// The item the `listed` rule steps from: the catalog's copy of the
/// cursor's item if it is still there, so that an item moved to another
/// place in the list carries on from its new place; otherwise the cursor's
/// own copy, so that a removed item carries on from where it used to be.
fn listed_from<'a>(catalog: &Catalog<'a>, from: &'a ItemKey) -> &'a ItemKey {
    catalog.get(&from.id).unwrap_or(from)
}
