//! What the rules engine knows about an item, and the natural order of items.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::natural::natural_cmp;

/// The parts of an item that programming rules look at.
///
/// The server builds one of these per item in a channel. Rules never see file
/// contents, durations, or playback state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    /// Stable server-assigned item ID. Must be unique within one call.
    pub id: String,
    /// Path used for natural sorting and for grouping by folder, with `/` as
    /// the separator. The server should make paths from different sources
    /// distinct (for example by prefixing the source ID) if items from
    /// several sources share a channel.
    pub path: String,
    /// Parsed show name, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<String>,
    /// Parsed season number, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    /// Parsed episode number, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    /// Where the channel's definition lists this item, for the `listed`
    /// rule: lower plays first. Items the definition lists together (such
    /// as the contents of one folder) share a position. Other rules ignore
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

impl ItemKey {
    /// The folder containing this item: everything in `path` before the last
    /// `/`, or the empty string for an item at the root.
    pub fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(dir, _)| dir)
    }

    /// Compares two items in natural order, the order the `ordered` rule
    /// plays them in:
    ///
    /// 1. show (natural sort; items without a show first),
    /// 2. season (numerically; items without a season after those with one),
    /// 3. episode (likewise),
    /// 4. path (natural sort, so `Ep 2` precedes `Ep 10`),
    /// 5. ID, so that the order is total even for duplicate metadata.
    pub fn natural_order(&self, other: &Self) -> Ordering {
        cmp_option_by(
            self.show.as_deref(),
            other.show.as_deref(),
            natural_cmp,
            true,
        )
        .then_with(|| cmp_option_by(self.season, other.season, |a, b| a.cmp(&b), false))
        .then_with(|| cmp_option_by(self.episode, other.episode, |a, b| a.cmp(&b), false))
        .then_with(|| natural_cmp(&self.path, &other.path))
        .then_with(|| self.id.cmp(&other.id))
    }

    /// Compares two items in listed order, the order the `listed` rule
    /// plays them in: by [`position`](Self::position) (items without one
    /// last), then in [natural order](Self::natural_order).
    pub fn listed_order(&self, other: &Self) -> Ordering {
        cmp_option_by(self.position, other.position, |a, b| a.cmp(&b), false)
            .then_with(|| self.natural_order(other))
    }
}

/// Compares two options with `cmp`, putting `None` first if `none_first`,
/// otherwise last.
pub(crate) fn cmp_option_by<T>(
    a: Option<T>,
    b: Option<T>,
    cmp: impl FnOnce(T, T) -> Ordering,
    none_first: bool,
) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => cmp(a, b),
        (None, None) => Ordering::Equal,
        (None, Some(_)) if none_first => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) if none_first => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, path: &str, season: Option<u32>, episode: Option<u32>) -> ItemKey {
        ItemKey {
            id: id.into(),
            path: path.into(),
            show: season.map(|_| "Show".into()),
            season,
            episode,
            position: None,
        }
    }

    #[test]
    fn folder_is_parent_path() {
        assert_eq!(
            item("a", "TV/Show/Season 1/e.mkv", None, None).folder(),
            "TV/Show/Season 1"
        );
        assert_eq!(item("a", "e.mkv", None, None).folder(), "");
    }

    #[test]
    fn season_and_episode_beat_path() {
        // Paths deliberately sort the opposite way to season/episode.
        let mut items = [
            item("d", "a", Some(2), Some(1)),
            item("c", "b", Some(1), Some(10)),
            item("b", "c", Some(1), Some(2)),
            item("a", "d", Some(1), Some(1)),
        ];
        items.sort_by(ItemKey::natural_order);
        let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d"]);
    }

    #[test]
    fn unparsed_items_sort_by_natural_path() {
        let mut items = [
            item("x", "Essays/Ep 10.mkv", None, None),
            item("y", "Essays/Ep 2.mkv", None, None),
        ];
        items.sort_by(ItemKey::natural_order);
        assert_eq!(items[0].id, "y");
    }

    #[test]
    fn missing_episode_sorts_after_numbered_ones() {
        let mut items = [
            item("special", "a", Some(1), None),
            item("e1", "z", Some(1), Some(1)),
        ];
        items.sort_by(ItemKey::natural_order);
        assert_eq!(items[0].id, "e1");
    }

    #[test]
    fn listed_order_puts_position_before_metadata() {
        let at = |id: &str, season, position| ItemKey {
            position,
            ..item(id, id, Some(season), Some(1))
        };
        let mut items = [
            at("unlisted", 1, None),
            at("s1-second", 1, Some(1)),
            at("s3-first", 3, Some(0)),
            at("s2-second", 2, Some(1)),
        ];
        items.sort_by(ItemKey::listed_order);
        let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
        // Shared positions fall back to natural order; no position is last.
        assert_eq!(ids, ["s3-first", "s1-second", "s2-second", "unlisted"]);
    }

    #[test]
    fn id_breaks_ties() {
        let a = item("a", "same", None, None);
        let b = item("b", "same", None, None);
        assert_eq!(a.natural_order(&b), Ordering::Less);
        assert_eq!(a.natural_order(&a), Ordering::Equal);
    }
}
