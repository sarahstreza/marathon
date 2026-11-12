//! A sorted, de-duplicated view of the items passed to one rules call.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::item::ItemKey;

/// How a [`Catalog`] sorts its items: [`ItemKey::natural_order`] or
/// [`ItemKey::listed_order`].
pub(crate) type SortOrder = fn(&ItemKey, &ItemKey) -> Ordering;

/// The items of one call to [`crate::next`] or [`crate::previous`], sorted
/// (in natural order, or listed order for the `listed` rule) with duplicate
/// IDs removed.
///
/// Sorting here makes every rule independent of the order the caller passed
/// the items in, which is what keeps sequences stable across rescans that
/// return the same items in a different order. (The `listed` rule's order
/// comes from [`ItemKey::position`], never from the slice order.)
pub(crate) struct Catalog<'a> {
    items: Vec<&'a ItemKey>,
    by_id: BTreeMap<&'a str, &'a ItemKey>,
}

impl<'a> Catalog<'a> {
    /// A catalog in natural order.
    pub(crate) fn new(items: &'a [ItemKey]) -> Self {
        Self::sorted_by(items, ItemKey::natural_order)
    }

    /// A catalog sorted by `order`.
    pub(crate) fn sorted_by(items: &'a [ItemKey], order: SortOrder) -> Self {
        let mut sorted: Vec<&ItemKey> = items.iter().collect();
        sorted.sort_by(|a, b| order(a, b));
        let mut by_id = BTreeMap::new();
        // If an ID appears twice, keep its first entry in sorted order.
        sorted.retain(|item| match by_id.entry(item.id.as_str()) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(*item);
                true
            }
        });
        Self {
            items: sorted,
            by_id,
        }
    }

    /// All items, in the catalog's order.
    pub(crate) fn items(&self) -> &[&'a ItemKey] {
        &self.items
    }

    pub(crate) fn get(&self, id: &str) -> Option<&'a ItemKey> {
        self.by_id.get(id).copied()
    }

    pub(crate) fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Every item ID, in the catalog's order.
    pub(crate) fn ids(&self) -> Vec<String> {
        ids(&self.items)
    }
}

/// The IDs of `items`, in the same order.
pub(crate) fn ids(items: &[&ItemKey]) -> Vec<String> {
    items.iter().map(|i| i.id.clone()).collect()
}

/// The first item of `sorted` (sorted by `order`) that comes after `from`.
///
/// `from` need not be in `sorted` (it may have been removed by a rescan); the
/// result is then the item that would have followed it. An entry with the
/// same ID as `from` is never returned, in case its metadata changed.
pub(crate) fn after<'a>(
    sorted: &[&'a ItemKey],
    from: &ItemKey,
    order: SortOrder,
) -> Option<&'a ItemKey> {
    sorted
        .iter()
        .find(|i| i.id != from.id && order(i, from) == Ordering::Greater)
        .copied()
}

/// The last item of `sorted` (sorted by `order`) that comes before `from`.
/// The mirror image of [`after`].
pub(crate) fn before<'a>(
    sorted: &[&'a ItemKey],
    from: &ItemKey,
    order: SortOrder,
) -> Option<&'a ItemKey> {
    sorted
        .iter()
        .rev()
        .find(|i| i.id != from.id && order(i, from) == Ordering::Less)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str, path: &str) -> ItemKey {
        ItemKey {
            id: id.into(),
            path: path.into(),
            show: None,
            season: None,
            episode: None,
            position: None,
        }
    }

    #[test]
    fn sorts_and_dedups() {
        let items = vec![key("b", "2"), key("a", "1"), key("b", "3")];
        let cat = Catalog::new(&items);
        assert_eq!(cat.ids(), ["a", "b"]);
        assert_eq!(cat.get("b").map(|i| i.path.as_str()), Some("2"));
    }

    #[test]
    fn listed_catalog_sorts_by_position() {
        let at = |id: &str, position| ItemKey {
            position: Some(position),
            ..key(id, id)
        };
        let items = vec![at("a", 2), at("b", 0), at("c", 1)];
        let cat = Catalog::sorted_by(&items, ItemKey::listed_order);
        assert_eq!(cat.ids(), ["b", "c", "a"]);
    }

    #[test]
    fn after_and_before_handle_absent_from() {
        let items = vec![key("a", "1"), key("c", "3")];
        let cat = Catalog::new(&items);
        let gone = key("b", "2");
        let natural = ItemKey::natural_order;
        let after = |from| after(cat.items(), from, natural).map(|i| i.id.as_str());
        let before = |from| before(cat.items(), from, natural).map(|i| i.id.as_str());
        assert_eq!(after(&gone), Some("c"));
        assert_eq!(before(&gone), Some("a"));
        assert_eq!(after(&items[1]), None);
        assert_eq!(before(&items[0]), None);
    }
}
