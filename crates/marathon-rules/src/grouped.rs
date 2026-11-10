//! The `grouped` rule: play one group at a time, ordering the groups and the
//! items within each group independently.
//!
//! Each level is either *ordered* or *shuffled*:
//!
//! - An ordered level keeps no state. Its position is derived from the
//!   current item (or its group) by natural order, so items or groups added
//!   by a rescan slot straight into place.
//! - A shuffled level is a [`Bag`]. For groups, one bag cycle is one pass
//!   over every group. For items, each visit to a group deals a new cycle
//!   over that group's items; the bag's cycle number counts visits, so every
//!   visit is shuffled differently.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ops::Bound;

use serde::{Deserialize, Serialize};

use crate::bag::Bag;
use crate::catalog::{self, Catalog};
use crate::engine::{DOMAIN_GROUPS, DOMAIN_WITHIN, State};
use crate::item::{ItemKey, cmp_option_by};
use crate::natural::natural_cmp;
use crate::{GroupBy, Order};

/// Identifies one group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GroupKey {
    /// Items with this parsed season (`None`: no season parsed).
    Season(Option<u32>),
    /// Items in this folder.
    Folder(String),
}

impl GroupKey {
    fn of(item: &ItemKey, by: GroupBy) -> Self {
        match by {
            GroupBy::Season => Self::Season(item.season),
            GroupBy::Folder => Self::Folder(item.folder().to_owned()),
        }
    }

    fn matches(&self, by: GroupBy) -> bool {
        matches!(
            (self, by),
            (Self::Season(_), GroupBy::Season) | (Self::Folder(_), GroupBy::Folder)
        )
    }
}

/// Natural order of groups: seasons numerically with "no season" last,
/// folders by natural sort of their path.
impl Ord for GroupKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Season(a), Self::Season(b)) => cmp_option_by(*a, *b, |a, b| a.cmp(&b), false),
            (Self::Folder(a), Self::Folder(b)) => natural_cmp(a, b),
            (Self::Season(_), Self::Folder(_)) => Ordering::Less,
            (Self::Folder(_), Self::Season(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for GroupKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Cursor state for the `grouped` rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GroupedState {
    /// The group being played.
    group: GroupKey,
    /// Present exactly when groups are shuffled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    groups: Option<Bag<GroupKey>>,
    /// Present exactly when items within a group are shuffled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    within: Option<Bag<String>>,
}

/// The catalog partitioned into groups, each in natural item order.
struct Groups<'a> {
    by: GroupBy,
    map: BTreeMap<GroupKey, Vec<&'a ItemKey>>,
}

impl<'a> Groups<'a> {
    fn new(catalog: &Catalog<'a>, by: GroupBy) -> Self {
        let mut map: BTreeMap<GroupKey, Vec<&ItemKey>> = BTreeMap::new();
        for item in catalog.items() {
            map.entry(GroupKey::of(item, by)).or_default().push(item);
        }
        Self { by, map }
    }

    fn keys(&self) -> Vec<GroupKey> {
        self.map.keys().cloned().collect()
    }

    fn members(&self, key: &GroupKey) -> Option<&[&'a ItemKey]> {
        self.map.get(key).map(Vec::as_slice)
    }

    fn contains(&self, key: &GroupKey) -> bool {
        self.map.contains_key(key)
    }

    /// Whether `id` is an item of group `key` right now.
    fn holds(&self, catalog: &Catalog<'_>, key: &GroupKey, id: &str) -> bool {
        catalog
            .get(id)
            .is_some_and(|item| GroupKey::of(item, self.by) == *key)
    }

    /// The group after `key` in natural order, wrapping to the first.
    fn after_wrapping(&self, key: &GroupKey) -> Option<&GroupKey> {
        self.map
            .range((Bound::Excluded(key), Bound::Unbounded))
            .next()
            .or_else(|| self.map.first_key_value())
            .map(|(k, _)| k)
    }

    /// The group before `key` in natural order, wrapping to the last.
    fn before_wrapping(&self, key: &GroupKey) -> Option<&GroupKey> {
        self.map
            .range(..key)
            .next_back()
            .or_else(|| self.map.last_key_value())
            .map(|(k, _)| k)
    }
}

type Outcome<'a> = (&'a ItemKey, State);

/// Returns `state` if it is usable as `grouped(by, groups, within)` state.
fn usable<'s>(
    state: &'s State,
    by: GroupBy,
    groups: Order,
    within: Order,
    from: Option<&ItemKey>,
) -> Option<&'s GroupedState> {
    let State::Grouped(st) = state else {
        return None;
    };
    let ok = st.group.matches(by)
        && st.groups.is_some() == (groups == Order::Shuffled)
        && st.within.is_some() == (within == Order::Shuffled)
        // An ordered within-group level is positioned by the current item.
        && (within == Order::Shuffled || from.is_some());
    ok.then_some(st)
}

/// `next` for the `grouped` rule.
pub(crate) fn next<'a>(
    by: GroupBy,
    groups: Order,
    within: Order,
    seed: u64,
    catalog: &Catalog<'a>,
    state: &State,
    from: Option<&ItemKey>,
) -> Option<Outcome<'a>> {
    let g = Groups::new(catalog, by);
    let st = match usable(state, by, groups, within, from) {
        Some(st) => st.clone(),
        None => match from.filter(|f| catalog.contains(&f.id)) {
            // Switched from another rule: rebuild state around the current
            // item, then continue after it.
            Some(f) => rebuild(seed, &g, groups, within, f)?,
            None => return begin(seed, catalog, &g, groups, within, from),
        },
    };

    // Next item in the same group, if any.
    match &st.within {
        None => {
            let next_in_group =
                from.and_then(|f| g.members(&st.group).and_then(|m| catalog::after(m, f)));
            if let Some(item) = next_in_group {
                return Some((item, State::Grouped(st)));
            }
        }
        Some(bag) => {
            if let Some(bag) = bag.forward(|id| g.holds(catalog, &st.group, id)) {
                let item = catalog.get(bag.current()?)?;
                let within = Some(bag);
                return Some((item, State::Grouped(GroupedState { within, ..st })));
            }
        }
    }

    // The group is finished: move to the next one.
    let (group, groups_bag) = match &st.groups {
        None => (g.after_wrapping(&st.group)?.clone(), None),
        Some(bag) => {
            let bag = bag
                .forward(|k| g.contains(k))
                .or_else(|| bag.next_cycle(seed, DOMAIN_GROUPS, &g.keys()))?;
            (bag.current()?.clone(), Some(bag))
        }
    };
    enter(
        seed,
        catalog,
        &g,
        group,
        groups_bag,
        within,
        st.within.as_ref(),
        from,
    )
}

/// `previous` for the `grouped` rule. Requires usable grouped state.
pub(crate) fn previous<'a>(
    by: GroupBy,
    groups: Order,
    within: Order,
    catalog: &Catalog<'a>,
    state: &State,
    from: &ItemKey,
) -> Option<Outcome<'a>> {
    let g = Groups::new(catalog, by);
    let st = usable(state, by, groups, within, Some(from))?.clone();

    // Previous item in the same group, if any.
    match &st.within {
        None => {
            if let Some(item) = g.members(&st.group).and_then(|m| catalog::before(m, from)) {
                return Some((item, State::Grouped(st)));
            }
        }
        Some(bag) => {
            if let Some(bag) = bag.backward(|id| g.holds(catalog, &st.group, id)) {
                let item = catalog.get(bag.current()?)?;
                let within = Some(bag);
                return Some((item, State::Grouped(GroupedState { within, ..st })));
            }
        }
    }

    // At the start of the group: go to the end of the previous one.
    let (group, groups_bag) = match &st.groups {
        None => (g.before_wrapping(&st.group)?.clone(), None),
        Some(bag) => {
            let present = |k: &GroupKey| g.contains(k);
            let bag = bag.backward(present).or_else(|| bag.prior_cycle(present))?;
            (bag.current()?.clone(), Some(bag))
        }
    };
    let (item, within_bag) = match &st.within {
        None => (*g.members(&group)?.last()?, None),
        Some(bag) => {
            let bag = bag.prior_cycle(|id| g.holds(catalog, &group, id))?;
            (catalog.get(bag.current()?)?, Some(bag))
        }
    };
    Some((
        item,
        State::Grouped(GroupedState {
            group,
            groups: groups_bag,
            within: within_bag,
        }),
    ))
}

/// Starts from scratch: the first group, and the first item in it. `avoid`
/// (an item that was playing under some other state) is kept from coming
/// first where a shuffle allows.
fn begin<'a>(
    seed: u64,
    catalog: &Catalog<'a>,
    g: &Groups<'a>,
    groups: Order,
    within: Order,
    avoid: Option<&ItemKey>,
) -> Option<Outcome<'a>> {
    let (group, groups_bag) = match groups {
        Order::Ordered => (g.map.first_key_value()?.0.clone(), None),
        Order::Shuffled => {
            let avoid_group = avoid.map(|a| GroupKey::of(a, g.by));
            let bag = Bag::generate(
                seed,
                DOMAIN_GROUPS,
                0,
                &g.keys(),
                avoid_group.as_ref(),
                None,
            )?;
            (bag.current()?.clone(), Some(bag))
        }
    };
    enter(seed, catalog, g, group, groups_bag, within, None, avoid)
}

/// Builds state positioned at `item` from scratch, as if `item` had just
/// played. Used when the cursor's state belongs to a different rule.
fn rebuild(
    seed: u64,
    g: &Groups<'_>,
    groups: Order,
    within: Order,
    item: &ItemKey,
) -> Option<GroupedState> {
    let group = GroupKey::of(item, g.by);
    let groups_bag = match groups {
        Order::Ordered => None,
        Order::Shuffled => Some(
            Bag::generate(seed, DOMAIN_GROUPS, 0, &g.keys(), None, None)?.positioned_at(&group)?,
        ),
    };
    let within_bag = match within {
        Order::Ordered => None,
        Order::Shuffled => {
            let ids = catalog::ids(g.members(&group)?);
            Some(Bag::generate(seed, DOMAIN_WITHIN, 0, &ids, None, None)?.positioned_at(&item.id)?)
        }
    };
    Some(GroupedState {
        group,
        groups: groups_bag,
        within: within_bag,
    })
}

/// Enters `group` at its first item.
///
/// For shuffled items, deals the group a new cycle that follows
/// `prev_within` (the bag of the group just finished), so the previous
/// group's order is kept for `previous`.
#[allow(clippy::too_many_arguments)]
fn enter<'a>(
    seed: u64,
    catalog: &Catalog<'a>,
    g: &Groups<'a>,
    group: GroupKey,
    groups_bag: Option<Bag<GroupKey>>,
    within: Order,
    prev_within: Option<&Bag<String>>,
    avoid: Option<&ItemKey>,
) -> Option<Outcome<'a>> {
    let members = g.members(&group)?;
    let (item, within_bag) = match within {
        Order::Ordered => (*members.first()?, None),
        Order::Shuffled => {
            let ids = catalog::ids(members);
            let bag = match prev_within {
                Some(prev) => prev.next_cycle(seed, DOMAIN_WITHIN, &ids),
                None => Bag::generate(seed, DOMAIN_WITHIN, 0, &ids, avoid.map(|a| &a.id), None),
            }?;
            (catalog.get(bag.current()?)?, Some(bag))
        }
    };
    Some((
        item,
        State::Grouped(GroupedState {
            group,
            groups: groups_bag,
            within: within_bag,
        }),
    ))
}
