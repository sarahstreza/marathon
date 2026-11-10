//! A sorted, de-duplicated view of the items passed to one rules call.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::item::ItemKey;

/// The items of one call to [`crate::next`] or [`crate::previous`], in
/// natural order with duplicate IDs removed.
///
/// Sorting here makes every rule independent of the order the caller listed
/// the items in, which is what keeps sequences stable across rescans that
/// return the same items in a different order.
pub(crate) struct Catalog<'a> {
    items: Vec<&'a ItemKey>,
    by_id: BTreeMap<&'a str, &'a ItemKey>,
}

impl<'a> Catalog<'a> {
    pub(crate) fn new(items: &'a [ItemKey]) -> Self {
        let mut sorted: Vec<&ItemKey> = items.iter().collect();
        sorted.sort_by(|a, b| a.natural_order(b));
        let mut by_id = BTreeMap::new();
        // If an ID appears twice, keep its first entry in natural order.
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

    /// All items, in natural order.
    pub(crate) fn items(&self) -> &[&'a ItemKey] {
        &self.items
    }

    pub(crate) fn get(&self, id: &str) -> Option<&'a ItemKey> {
        self.by_id.get(id).copied()
    }

    pub(crate) fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Every item ID, in natural order of the items.
    pub(crate) fn ids(&self) -> Vec<String> {
        ids(&self.items)
    }
}

/// The IDs of `items`, in the same order.
pub(crate) fn ids(items: &[&ItemKey]) -> Vec<String> {
    items.iter().map(|i| i.id.clone()).collect()
}

/// The first item of `sorted` that comes after `from` in natural order.
///
/// `from` need not be in `sorted` (it may have been removed by a rescan); the
/// result is then the item that would have followed it. An entry with the
/// same ID as `from` is never returned, in case its metadata changed.
pub(crate) fn after<'a>(sorted: &[&'a ItemKey], from: &ItemKey) -> Option<&'a ItemKey> {
    sorted
        .iter()
        .find(|i| i.id != from.id && i.natural_order(from) == Ordering::Greater)
        .copied()
}

/// The last item of `sorted` that comes before `from` in natural order. The
/// mirror image of [`after`].
pub(crate) fn before<'a>(sorted: &[&'a ItemKey], from: &ItemKey) -> Option<&'a ItemKey> {
    sorted
        .iter()
        .rev()
        .find(|i| i.id != from.id && i.natural_order(from) == Ordering::Less)
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
    fn after_and_before_handle_absent_from() {
        let items = vec![key("a", "1"), key("c", "3")];
        let cat = Catalog::new(&items);
        let gone = key("b", "2");
        assert_eq!(after(cat.items(), &gone).map(|i| i.id.as_str()), Some("c"));
        assert_eq!(before(cat.items(), &gone).map(|i| i.id.as_str()), Some("a"));
        assert!(after(cat.items(), &items[1]).is_none());
        assert!(before(cat.items(), &items[0]).is_none());
    }
}
