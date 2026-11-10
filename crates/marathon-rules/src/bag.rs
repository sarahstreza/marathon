//! Bag shuffle state: one shuffled cycle through a set of members.
//!
//! A [`Bag`] stores the explicit order of its current cycle rather than
//! re-deriving it from the seed, so library rescans cannot reshuffle a cycle
//! that is already under way: members that disappear are skipped, and new
//! members join when the next cycle is generated.
//!
//! The previous cycle's order is kept (one level only) so that `previous` can
//! step back across a cycle boundary. Going further back than that is not
//! supported for shuffles.

use serde::{Deserialize, Serialize};

use crate::rng::Rng;

/// One cycle of a bag shuffle and the position within it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Bag<T> {
    /// Cycle number; selects the RNG stream the order was generated from.
    pub(crate) cycle: u64,
    /// Every member of this cycle, in play order.
    pub(crate) order: Vec<T>,
    /// Index into `order` of the member currently playing.
    pub(crate) pos: usize,
    /// The previous cycle's order, for stepping back across a cycle boundary.
    /// (No `serde(default)`: it would demand `T: Default`, and serde already
    /// reads a missing `Option` field as `None`.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prior: Option<Vec<T>>,
}

impl<T: Clone + PartialEq> Bag<T> {
    /// Generates cycle number `cycle` over `members`.
    ///
    /// `members` must be in a canonical order (independent of how the caller
    /// happened to list them) for the result to be reproducible. If `avoid`
    /// would come first and there is any other member, it is swapped away
    /// from the front, so that a cycle never starts with the member that
    /// ended the previous one.
    ///
    /// Returns `None` if `members` is empty.
    pub(crate) fn generate(
        seed: u64,
        domain: u64,
        cycle: u64,
        members: &[T],
        avoid: Option<&T>,
        prior: Option<Vec<T>>,
    ) -> Option<Self> {
        if members.is_empty() {
            return None;
        }
        let mut rng = Rng::new(seed, domain, cycle);
        let mut order = members.to_vec();
        rng.shuffle(&mut order);
        if order.len() >= 2 && avoid.is_some_and(|a| order.first() == Some(a)) {
            let j = 1 + rng.index(order.len() - 1);
            order.swap(0, j);
        }
        Some(Self {
            cycle,
            order,
            pos: 0,
            prior,
        })
    }

    /// The member at the current position.
    pub(crate) fn current(&self) -> Option<&T> {
        self.order.get(self.pos)
    }

    /// Moves the position to `member`, if it is in this cycle.
    pub(crate) fn positioned_at(mut self, member: &T) -> Option<Self> {
        self.pos = self.order.iter().position(|m| m == member)?;
        Some(self)
    }

    /// The next member of this cycle for which `present` holds, or `None` if
    /// the cycle is used up.
    pub(crate) fn forward(&self, present: impl Fn(&T) -> bool) -> Option<Self> {
        let start = self.pos.saturating_add(1);
        let pos = (start..self.order.len()).find(|&i| self.order.get(i).is_some_and(&present))?;
        Some(Self {
            pos,
            ..self.clone()
        })
    }

    /// The previous member of this cycle for which `present` holds, or `None`
    /// at the start of the cycle.
    pub(crate) fn backward(&self, present: impl Fn(&T) -> bool) -> Option<Self> {
        let end = self.pos.min(self.order.len());
        let pos = (0..end)
            .rev()
            .find(|&i| self.order.get(i).is_some_and(&present))?;
        Some(Self {
            pos,
            ..self.clone()
        })
    }

    /// Starts the following cycle over `members`, remembering this cycle's
    /// order as the prior one.
    pub(crate) fn next_cycle(&self, seed: u64, domain: u64, members: &[T]) -> Option<Self> {
        Self::generate(
            seed,
            domain,
            self.cycle.wrapping_add(1),
            members,
            self.current(),
            Some(self.order.clone()),
        )
    }

    /// Returns to the last present member of the prior cycle, if it is known.
    pub(crate) fn prior_cycle(&self, present: impl Fn(&T) -> bool) -> Option<Self> {
        let order = self.prior.clone()?;
        let pos = order.iter().rposition(present)?;
        Some(Self {
            cycle: self.cycle.wrapping_sub(1),
            order,
            pos,
            prior: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: u64 = 99;

    fn members(n: u32) -> Vec<u32> {
        (0..n).collect()
    }

    fn walk(mut bag: Bag<u32>, all: &[u32], steps: usize) -> (Vec<u32>, Bag<u32>) {
        let mut out = vec![*bag.current().unwrap()];
        for _ in 1..steps {
            bag = bag
                .forward(|_| true)
                .or_else(|| bag.next_cycle(1, D, all))
                .unwrap();
            out.push(*bag.current().unwrap());
        }
        (out, bag)
    }

    #[test]
    fn empty_members_make_no_bag() {
        assert!(Bag::<u32>::generate(1, D, 0, &[], None, None).is_none());
    }

    #[test]
    fn cycles_are_permutations_and_do_not_repeat_at_the_seam() {
        for n in 2..12 {
            let all = members(n);
            let bag = Bag::generate(1, D, 0, &all, None, None).unwrap();
            let (seq, _) = walk(bag, &all, all.len() * 20);
            for cycle in seq.chunks(all.len()) {
                let mut sorted = cycle.to_vec();
                sorted.sort_unstable();
                assert_eq!(sorted, all);
            }
            for pair in seq.chunks(all.len()).collect::<Vec<_>>().windows(2) {
                assert_ne!(pair[0].last(), pair[1].first());
            }
        }
    }

    #[test]
    fn forward_and_backward_skip_absent_members() {
        let bag = Bag {
            cycle: 0,
            order: vec![10, 11, 12, 13],
            pos: 1,
            prior: None,
        };
        let fwd = bag.forward(|m| *m != 12).unwrap();
        assert_eq!(fwd.current(), Some(&13));
        assert!(fwd.forward(|_| true).is_none());
        let back = fwd.backward(|m| *m != 12).unwrap();
        assert_eq!(back.current(), Some(&11));
        assert!(bag.backward(|m| *m != 10).is_none());
    }

    #[test]
    fn prior_cycle_round_trips() {
        let all = members(5);
        let bag = Bag::generate(7, D, 0, &all, None, None)
            .unwrap()
            .positioned_at(&all[0])
            .unwrap();
        let last = Bag {
            pos: all.len() - 1,
            ..bag
        };
        let next = last.next_cycle(7, D, &all).unwrap();
        let back = next.prior_cycle(|_| true).unwrap();
        assert_eq!(
            back,
            Bag {
                prior: None,
                ..last.clone()
            }
        );
        // Going forward again regenerates the identical cycle.
        assert_eq!(back.next_cycle(7, D, &all).unwrap(), next);
        // Only one level of history is kept.
        assert!(back.prior_cycle(|_| true).is_none());
    }

    #[test]
    fn out_of_range_position_does_not_panic() {
        let bag = Bag {
            cycle: 0,
            order: vec![1, 2],
            pos: usize::MAX,
            prior: None,
        };
        assert!(bag.current().is_none());
        assert!(bag.forward(|_| true).is_none());
        assert_eq!(bag.backward(|_| true).unwrap().current(), Some(&2));
    }
}
