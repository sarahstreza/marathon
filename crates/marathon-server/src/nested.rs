//! Resolving channels to items, through any nesting of child channels
//! (PLAN.md M11).
//!
//! A channel's *entries* are its playable items plus one entry per child
//! channel. The rules engine picks among entries without knowing which is
//! which ([`child_key`] makes a child look like any other item). When the
//! pick is a child channel, the child is resolved in turn, down to an item.
//!
//! # Where state lives
//!
//! Every channel reached during resolution has a [`NodeState`]: its rule
//! cursor (whose current item is the current entry), the item it resolves
//! to, and the saved position (meaningful only where the current entry is
//! an item). States are addressed by an [`Addr`]:
//!
//! - `"office"`: the channel's own state, its `channel_state` row.
//! - `"essayists>contrapoints"`: the state that the independent parent
//!   `essayists` keeps for its child `contrapoints`, stored in the
//!   parent's row (`children_json`, key `contrapoints`).
//!
//! A child's address depends on its parent's `state` setting. A *shared*
//! child lives in the same scope as its parent (at the top level, its own
//! row), so watching it through the parent or directly is one position. An
//! *independent* parent opens a new scope under its own address, so its
//! children's own rows are never read or written. Scopes nest:
//! `"a>b>c"` is `c` as kept by independent `b` as kept by independent `a`,
//! all stored in `a`'s row under `b>c`.
//!
//! # Moving
//!
//! [`Resolver::ensure`] makes sure a channel has a valid current entry and
//! resolves it to an item, starting channels never played and stepping past
//! entries that are gone. [`Resolver::advance`] moves a channel on: first
//! the child it is playing (recursively), then its own rule.
//! [`Resolver::previous`] undoes that: its own rule steps back, and if that
//! lands on a child, the child steps back too.
//!
//! Everything here works on preloaded data ([`Data`]) and an in-memory
//! copy of the state rows, and never touches the database itself:
//! `playback` loads, calls, and saves the rows marked dirty. A clone of a
//! [`Resolver`] is a dry run, which is how `next` and plans are computed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use marathon_api::PLAN_UNKNOWN_DURATION_MS;
use marathon_rules::{Cursor, ItemKey, Rule, Step, WeightBy};
use serde::{Deserialize, Serialize};

use crate::config::{ChannelConfig, ChildEntry, ItemSelector, StateMode};
use crate::db::ItemRow;

/// How many child channels with nothing to play one request steps past
/// before giving up, matching the client's consecutive-failure cap
/// (PLAN.md M3.5). Counted across the whole resolution chain.
pub(crate) const MAX_SKIPPED_CHILDREN: u32 = 5;

/// Prefix of a child channel entry's ID. Item IDs are UUIDs, so they
/// cannot collide.
const CHILD_PREFIX: &str = "channel:";

/// The address of a channel's state; see the module docs.
pub(crate) type Addr = String;

/// One channel's playback state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NodeState {
    /// The item the channel resolves to, when last resolved. For a channel
    /// on a child entry this is a cache for people reading the database;
    /// resolution always goes through the child.
    #[serde(default)]
    pub item_id: Option<String>,
    /// The saved position within the item, when the current entry is an
    /// item.
    #[serde(default)]
    pub position_ms: i64,
    pub cursor: Cursor,
}

/// One `channel_state` row: a channel's own state and the states it keeps
/// for descendants as an independent parent, keyed by address relative to
/// the row's channel (`contrapoints`, `b>c`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Row {
    pub own: Option<NodeState>,
    pub children: BTreeMap<String, NodeState>,
    /// Whether this request changed the row, so it must be saved.
    pub dirty: bool,
}

/// What one channel is made of, as loaded for a request.
#[derive(Debug, Clone)]
pub(crate) struct ChannelData {
    pub config: ChannelConfig,
    /// The channel's own playable items (not its children's).
    pub items: Vec<ItemRow>,
    /// Each item's place in the channel's list, by item ID, for the
    /// `listed` rule (`channel_items.position`).
    pub positions: HashMap<String, u32>,
}

/// Every channel a request may reach: the requested one and its
/// descendants.
#[derive(Debug, Clone, Default)]
pub(crate) struct Data {
    pub channels: HashMap<String, ChannelData>,
}

/// One step of a resolution path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hop {
    pub channel_id: String,
    pub addr: Addr,
}

/// Where a channel resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// The channel asked about first, then each child channel down to the
    /// one `item` belongs to.
    pub chain: Vec<Hop>,
    pub item: ItemRow,
    pub position_ms: i64,
}

impl Resolved {
    /// The channel `item` belongs to.
    pub fn owner(&self) -> &Hop {
        // `ensure` always builds a chain of at least the channel itself.
        self.chain
            .last()
            .unwrap_or_else(|| unreachable!("a resolution chain is never empty"))
    }
}

/// Why a channel cannot be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unresolved {
    /// The channel (named) has no entries, or every entry is a child with
    /// nothing to play.
    Empty(String),
    /// [`MAX_SKIPPED_CHILDREN`] children with nothing to play were skipped
    /// during this request.
    TooManySkipped,
}

/// A channel's entry, looked up by its rules-engine ID.
enum EntryRef<'a> {
    Item(&'a ItemRow),
    Child(&'a ChildEntry),
}

/// The rules engine's view of an item. The path is prefixed with the
/// source ID so that items from different sources in one channel stay
/// distinct.
pub(crate) fn item_key(row: &ItemRow) -> ItemKey {
    ItemKey {
        id: row.id.clone(),
        path: format!("{}/{}", row.source_id, row.path),
        show: row.show.clone(),
        season: row.season.and_then(|s| u32::try_from(s).ok()),
        episode: row.episode.and_then(|e| u32::try_from(e).ok()),
        position: None,
    }
}

/// The rules engine's view of a child channel entry: an item with no
/// metadata at `channels/<id>`, so the `ordered` rule plays child channels
/// in order of ID, and grouping by folder puts them all in one group.
pub(crate) fn child_key(channel_id: &str) -> ItemKey {
    ItemKey {
        id: format!("{CHILD_PREFIX}{channel_id}"),
        path: format!("channels/{channel_id}"),
        show: None,
        season: None,
        episode: None,
        position: None,
    }
}

/// The address of `child`'s state when reached from `parent_addr`.
pub(crate) fn child_addr(parent_addr: &str, mode: StateMode, child: &str) -> Addr {
    match mode {
        StateMode::Independent => format!("{parent_addr}>{child}"),
        StateMode::Shared => match parent_addr.rsplit_once('>') {
            Some((scope, _)) => format!("{scope}>{child}"),
            None => child.to_owned(),
        },
    }
}

/// Splits an address into the row it is stored in and the key within that
/// row's children (`None` for the row's own state).
fn split_addr(addr: &str) -> (&str, Option<&str>) {
    match addr.split_once('>') {
        Some((row, rest)) => (row, Some(rest)),
        None => (addr, None),
    }
}

/// A seed for a channel state that is being created as part of resolving
/// a parent: derived from the parent's seed and the state's address, so a
/// dry run (a plan, or `next`) predicts exactly the seed a live run will
/// pick later. (SplitMix64's mixer over FNV-1a of the address.)
fn derived_seed(parent_seed: u64, addr: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in addr.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut z = parent_seed ^ h;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Resolves and moves channels over [`Data`] and an in-memory copy of the
/// state rows. Cloning one makes a dry run.
#[derive(Clone)]
pub(crate) struct Resolver<'a> {
    data: &'a Data,
    rows: HashMap<String, Row>,
    /// Where a channel resolved from scratch gets its seed; the server
    /// passes a random source.
    fresh_seed: fn() -> u64,
    skipped: u32,
    warnings: Vec<String>,
}

impl<'a> Resolver<'a> {
    pub fn new(data: &'a Data, rows: HashMap<String, Row>, fresh_seed: fn() -> u64) -> Self {
        Self {
            data,
            rows,
            fresh_seed,
            skipped: 0,
            warnings: Vec::new(),
        }
    }

    /// A copy to try things on without affecting this one, with its own
    /// count of skipped children and warnings.
    pub fn dry_run(&self) -> Self {
        Self {
            skipped: 0,
            warnings: Vec::new(),
            ..self.clone()
        }
    }

    /// The rows, for saving those marked dirty.
    pub fn into_rows(self) -> HashMap<String, Row> {
        self.rows
    }

    /// Problems stepped past so far, such as children with nothing to play.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    fn get(&self, addr: &str) -> Option<&NodeState> {
        let (row, key) = split_addr(addr);
        let row = self.rows.get(row)?;
        match key {
            None => row.own.as_ref(),
            Some(key) => row.children.get(key),
        }
    }

    fn put(&mut self, addr: &str, state: NodeState) {
        let (row, key) = split_addr(addr);
        let row = self.rows.entry(row.to_owned()).or_default();
        let slot = match key {
            None => &mut row.own,
            Some(key) => {
                row.children.insert(key.to_owned(), state);
                row.dirty = true;
                return;
            }
        };
        *slot = Some(state);
        row.dirty = true;
    }

    fn channel(&self, id: &str) -> Result<&'a ChannelData, Unresolved> {
        self.data
            .channels
            .get(id)
            .ok_or_else(|| Unresolved::Empty(id.to_owned()))
    }

    /// The rules engine's entries for a channel: its items, then its
    /// children, each with its place in the channel's list.
    fn keys(channel: &ChannelData) -> Vec<ItemKey> {
        let items = channel.items.iter().map(|row| ItemKey {
            position: channel.positions.get(&row.id).copied(),
            ..item_key(row)
        });
        let children = channel.config.children().map(|c| ItemKey {
            position: channel.config.child_position(&c.channel),
            ..child_key(&c.channel)
        });
        items.chain(children).collect()
    }

    fn entry<'c>(channel: &'c ChannelData, id: &str) -> Option<EntryRef<'c>> {
        match id.strip_prefix(CHILD_PREFIX) {
            Some(child) => channel
                .config
                .children()
                .find(|c| c.channel == child)
                .map(EntryRef::Child),
            None => channel
                .items
                .iter()
                .find(|i| i.id == id)
                .map(EntryRef::Item),
        }
    }

    /// The rule's next step from `cursor`, weighing entries as the rule
    /// asks.
    fn step_next(&self, channel: &ChannelData, cursor: &Cursor) -> Option<Step> {
        let keys = Self::keys(channel);
        let weight = |key: &ItemKey| self.weight(channel, key);
        marathon_rules::next_weighted(&channel.config.rule, &keys, Some(cursor), &weight)
    }

    /// An entry's weight under a `weighted` rule (1 under any other).
    fn weight(&self, channel: &ChannelData, key: &ItemKey) -> u64 {
        let Rule::Weighted { by } = channel.config.rule else {
            return 1;
        };
        match (Self::entry(channel, &key.id), by) {
            (None, _) => 0,
            (Some(EntryRef::Item(item)), WeightBy::Manual) => {
                selector_weight(channel.config.selectors(), item)
            }
            (Some(EntryRef::Item(_)), WeightBy::ItemCount) => 1,
            (Some(EntryRef::Item(item)), WeightBy::Duration) => duration_of(item),
            (Some(EntryRef::Child(child)), WeightBy::Manual) => child.weight.unwrap_or(1),
            (Some(EntryRef::Child(child)), WeightBy::ItemCount) => {
                self.leaf_items(&child.channel).len() as u64
            }
            (Some(EntryRef::Child(child)), WeightBy::Duration) => self
                .leaf_items(&child.channel)
                .values()
                .map(|item| duration_of(item))
                .sum(),
        }
    }

    /// Every distinct playable item reachable from a channel, through all
    /// of its children, by ID.
    pub fn leaf_items(&self, channel_id: &str) -> BTreeMap<&'a str, &'a ItemRow> {
        let mut out = BTreeMap::new();
        let mut stack = vec![channel_id];
        let mut seen = BTreeSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(channel) = self.data.channels.get(id) else {
                continue;
            };
            for item in &channel.items {
                out.insert(item.id.as_str(), item);
            }
            stack.extend(channel.config.children().map(|c| c.channel.as_str()));
        }
        out
    }

    /// Makes sure the channel `id`, with its state at `addr`, has a valid
    /// current entry, and resolves it to an item. A channel never played
    /// starts at its rule's first entry (seeded from `parent_seed` if it is
    /// being reached through a parent); one whose current entry is gone
    /// moves on to the next; a child with nothing to play is skipped, with
    /// a warning.
    pub fn ensure(
        &mut self,
        addr: &str,
        id: &str,
        parent_seed: Option<u64>,
    ) -> Result<Resolved, Unresolved> {
        let channel = self.channel(id)?;
        let entry_count = Self::keys(channel).len();
        if entry_count == 0 {
            return Err(Unresolved::Empty(id.to_owned()));
        }

        let saved = self.get(addr).cloned();
        let mut state = match saved {
            Some(state)
                if state
                    .cursor
                    .current()
                    .is_some_and(|c| Self::entry(channel, &c.id).is_some()) =>
            {
                state
            }
            saved => {
                // First time, or the current entry is gone: step on from
                // wherever the cursor was.
                let cursor = match &saved {
                    Some(state) => state.cursor.clone(),
                    None => Cursor::seeded(match parent_seed {
                        Some(seed) => derived_seed(seed, addr),
                        None => (self.fresh_seed)(),
                    }),
                };
                let step = self
                    .step_next(channel, &cursor)
                    .ok_or_else(|| Unresolved::Empty(id.to_owned()))?;
                if let Some(old) = saved.as_ref().and_then(|s| s.cursor.current()) {
                    tracing::info!(channel = id, from = %old.id, to = %step.item.id,
                        "current entry is no longer playable; moving on");
                }
                let state = NodeState {
                    item_id: None,
                    position_ms: 0,
                    cursor: step.cursor,
                };
                self.put(addr, state.clone());
                state
            }
        };

        let mut tried = BTreeSet::new();
        loop {
            let current = state
                .cursor
                .current()
                .map(|c| c.id.clone())
                .unwrap_or_default();
            let resolved = match Self::entry(channel, &current) {
                Some(EntryRef::Item(item)) => Ok(Resolved {
                    chain: Vec::new(),
                    item: item.clone(),
                    position_ms: state.position_ms,
                }),
                Some(EntryRef::Child(child)) => {
                    let addr = child_addr(addr, channel.config.state, &child.channel);
                    self.ensure(&addr, &child.channel, Some(state.cursor.seed()))
                }
                // `next` only returns entries it was given.
                None => Err(Unresolved::Empty(current.clone())),
            };
            match resolved {
                Ok(mut resolved) => {
                    resolved.chain.insert(
                        0,
                        Hop {
                            channel_id: id.to_owned(),
                            addr: addr.to_owned(),
                        },
                    );
                    if state.item_id.as_deref() != Some(resolved.item.id.as_str()) {
                        state.item_id = Some(resolved.item.id.clone());
                        self.put(addr, state);
                    }
                    return Ok(resolved);
                }
                Err(Unresolved::TooManySkipped) => return Err(Unresolved::TooManySkipped),
                Err(Unresolved::Empty(empty)) => {
                    self.skipped += 1;
                    let message =
                        format!("channel {empty:?} has nothing to play; channel {id:?} skipped it");
                    tracing::warn!(%message, skipped = self.skipped);
                    self.warnings.push(message);
                    if self.skipped >= MAX_SKIPPED_CHILDREN {
                        return Err(Unresolved::TooManySkipped);
                    }
                    tried.insert(current);
                    if tried.len() >= entry_count {
                        return Err(Unresolved::Empty(id.to_owned()));
                    }
                    let step = self
                        .step_next(channel, &state.cursor)
                        .ok_or_else(|| Unresolved::Empty(id.to_owned()))?;
                    state = NodeState {
                        item_id: None,
                        position_ms: 0,
                        cursor: step.cursor,
                    };
                    self.put(addr, state.clone());
                }
            }
        }
    }

    /// Moves a channel on: if it is playing a child, the child moves on
    /// first (and so on down), then the channel's own rule picks its next
    /// entry. Call [`Resolver::ensure`] before (so there is a current
    /// entry) and after (to resolve the new one).
    pub fn advance(&mut self, addr: &str, id: &str) {
        let Ok(channel) = self.channel(id) else {
            return;
        };
        let Some(state) = self.get(addr).cloned() else {
            return;
        };
        if let Some(EntryRef::Child(child)) = state
            .cursor
            .current()
            .and_then(|c| Self::entry(channel, &c.id))
        {
            let child_addr = child_addr(addr, channel.config.state, &child.channel);
            self.advance(&child_addr, &child.channel);
        }
        if let Some(step) = self.step_next(channel, &state.cursor) {
            self.put(
                addr,
                NodeState {
                    item_id: None,
                    position_ms: 0,
                    cursor: step.cursor,
                },
            );
        }
    }

    /// Steps a channel back, undoing [`Resolver::advance`]: the channel's
    /// rule steps back, and if that lands on a child channel, the child
    /// steps back too (it moved on when the channel left it). Returns
    /// whether the channel moved. Call [`Resolver::ensure`] afterwards.
    pub fn previous(&mut self, addr: &str, id: &str) -> bool {
        let Ok(channel) = self.channel(id) else {
            return false;
        };
        let Some(state) = self.get(addr).cloned() else {
            return false;
        };
        let keys = Self::keys(channel);
        let Some(step) = marathon_rules::previous(&channel.config.rule, &keys, &state.cursor)
        else {
            return false;
        };
        let landed = step.item.id.clone();
        self.put(
            addr,
            NodeState {
                item_id: None,
                position_ms: 0,
                cursor: step.cursor,
            },
        );
        if let Some(EntryRef::Child(child)) = Self::entry(channel, &landed) {
            let child_addr = child_addr(addr, channel.config.state, &child.channel);
            self.previous(&child_addr, &child.channel);
        }
        true
    }

    /// Saves a playback position on the state that owns the item.
    pub fn set_position(&mut self, owner: &Hop, position_ms: i64) {
        if let Some(mut state) = self.get(&owner.addr).cloned() {
            state.position_ms = position_ms;
            self.put(&owner.addr, state);
        }
    }
}

/// The manual weight of an item: that of the first selector that selects
/// it, default 1.
fn selector_weight<'s>(selectors: impl Iterator<Item = &'s ItemSelector>, item: &ItemRow) -> u64 {
    selectors
        .filter(|s| s.source == item.source_id && s.matches(&item.path))
        .find_map(|s| s.weight)
        .unwrap_or(1)
}

/// An item's duration for weighing and planning; unknown durations count
/// as [`PLAN_UNKNOWN_DURATION_MS`].
pub(crate) fn duration_of(item: &ItemRow) -> u64 {
    item.duration_ms
        .and_then(|d| u64::try_from(d).ok())
        .filter(|d| *d > 0)
        .unwrap_or(PLAN_UNKNOWN_DURATION_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_addresses_follow_the_state_mode() {
        use StateMode::{Independent, Shared};
        assert_eq!(child_addr("a", Shared, "b"), "b");
        assert_eq!(child_addr("a", Independent, "b"), "a>b");
        assert_eq!(child_addr("a>b", Shared, "c"), "a>c");
        assert_eq!(child_addr("a>b", Independent, "c"), "a>b>c");
        assert_eq!(split_addr("a"), ("a", None));
        assert_eq!(split_addr("a>b>c"), ("a", Some("b>c")));
    }

    #[test]
    fn derived_seeds_differ_by_parent_and_address() {
        let s = derived_seed(1, "a>b");
        assert_eq!(s, derived_seed(1, "a>b"));
        assert_ne!(s, derived_seed(2, "a>b"));
        assert_ne!(s, derived_seed(1, "a>c"));
    }
}
