//! The `weighted` rule: pick each item at random in proportion to a weight,
//! never the same item twice in a row.
//!
//! The rule is generic: it knows nothing about what an item is or where its
//! weight comes from. The caller supplies a weight per item (see
//! [`crate::next_weighted`]); [`crate::WeightBy`] only records, for the
//! caller's benefit, how those weights are meant to be worked out.
//!
//! # Getting the proportions right without repeats
//!
//! Picking in proportion to the weights but excluding the item just played
//! skews the long-run frequencies toward uniform: with weights `1, 1, 2` the
//! heavy item would get 40% of picks instead of 50%. To compensate, picks
//! are made in proportion to adjusted weights `v` instead. When the next
//! pick is drawn from every item except the current one `i`, in proportion
//! to `v`, item `j` is chosen with probability `v_j / (1 - v_i)` (with `v`
//! normalised to sum to 1). That Markov chain's stationary distribution is
//! proportional to `v_j (1 - v_j)`, so solving
//!
//! ```text
//! v_j (1 - v_j) = c · w_j / W,   Σ v_j = 1
//! ```
//!
//! for `v` (and the constant `c`) makes each item's long-run share exactly
//! its weight's share `w_j / W`. The solution exists whenever no single
//! item has half the total weight or more. When one does, no sequence
//! without repeats can give it its share, so it plays every other time
//! (which is exactly its share when it has exactly half).
//!
//! All of this is plain `f64` arithmetic (`+ - * /` and `sqrt`, which IEEE
//! 754 defines exactly), so it gives the same picks on every platform.

use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::engine::DOMAIN_WEIGHTED;
use crate::item::ItemKey;
use crate::rng::Rng;

/// How many past picks a cursor remembers, for `previous`.
const HISTORY: usize = 50;

/// Cursor state for the `weighted` rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WeightedState {
    /// How many picks have been made; numbers the RNG stream of the next
    /// pick, so that stepping back and forward again repeats it exactly.
    picks: u64,
    /// IDs of the items played before the current one, oldest first, at
    /// most [`HISTORY`] of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<String>,
}

/// Picks the item after `from`. `state` is `None` if the cursor was made by
/// another rule (or is at the start).
pub(crate) fn next<'a>(
    catalog: &Catalog<'a>,
    weight: &dyn Fn(&ItemKey) -> u64,
    seed: u64,
    state: Option<&WeightedState>,
    from: Option<&ItemKey>,
) -> Option<(&'a ItemKey, WeightedState)> {
    let items = catalog.items();
    let weights = effective_weights(items, weight);
    let shares = adjusted_shares(&weights);

    let picks = state.map_or(0, |s| s.picks);
    let last = from.and_then(|f| items.iter().position(|i| i.id == f.id));
    let eligible = weights.iter().filter(|w| **w > 0).count();
    let excluded = last.filter(|_| eligible >= 2);

    let total: f64 = shares
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != excluded)
        .map(|(_, s)| s)
        .sum();
    let mut rng = Rng::new(seed, DOMAIN_WEIGHTED, picks);
    let mut target = unit_interval(rng.next_u64()) * total;
    let mut chosen = None;
    for (i, share) in shares.iter().enumerate() {
        if Some(i) == excluded || *share <= 0.0 {
            continue;
        }
        chosen = Some(i);
        if target < *share {
            break;
        }
        target -= share;
    }
    // Rounding can leave `target` a hair above the last share; `chosen` is
    // then the last eligible item, which is the right answer.
    let item = *items.get(chosen?)?;

    let mut history = state.map(|s| s.history.clone()).unwrap_or_default();
    if let Some(f) = from {
        history.push(f.id.clone());
        if history.len() > HISTORY {
            history.drain(..history.len() - HISTORY);
        }
    }
    Some((
        item,
        WeightedState {
            picks: picks.wrapping_add(1),
            history,
        },
    ))
}

/// The item played before the current one, if it is remembered and still
/// present.
pub(crate) fn previous<'a>(
    catalog: &Catalog<'a>,
    state: &WeightedState,
) -> Option<(&'a ItemKey, WeightedState)> {
    let mut history = state.history.clone();
    let mut picks = state.picks;
    while let Some(id) = history.pop() {
        picks = picks.wrapping_sub(1);
        if let Some(item) = catalog.get(&id) {
            return Some((item, WeightedState { picks, history }));
        }
    }
    None
}

/// Each item's weight, with zero weights kept only if every weight is
/// zero, in which case all items count equally.
fn effective_weights(items: &[&ItemKey], weight: &dyn Fn(&ItemKey) -> u64) -> Vec<u64> {
    let weights: Vec<u64> = items.iter().map(|i| weight(i)).collect();
    if weights.iter().all(|w| *w == 0) {
        vec![1; items.len()]
    } else {
        weights
    }
}

/// A `u64` mapped onto `[0, 1)` using its top 53 bits.
fn unit_interval(x: u64) -> f64 {
    // 53 bits fit an f64 mantissa exactly.
    (x >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

/// The adjusted shares `v` described in the module docs, summing to 1
/// (or all zero if there are no items).
///
/// Each `v_j` solves `v_j (1 - v_j) = c · s_j`, where `s` are the weight
/// shares. Every item but the heaviest takes the smaller root,
/// `(1 - sqrt(1 - 4 c s_j)) / 2`; the heaviest may need the larger one
/// (above 1/2). Parametrising by the heaviest item's `v_max` gives
/// `c = v_max (1 - v_max) / s_max`, and the total
/// `h(v_max) = v_max + Σ_others v_j(c)` runs from 0 (as `v_max` → 0) to
/// just above 1 (as `v_max` → 1) whenever `s_max < 1/2`, so bisection on
/// `h(v_max) = 1` finds a solution.
fn adjusted_shares(weights: &[u64]) -> Vec<f64> {
    let total: f64 = weights.iter().map(|w| *w as f64).sum();
    if total <= 0.0 {
        return vec![0.0; weights.len()];
    }
    let shares: Vec<f64> = weights.iter().map(|w| *w as f64 / total).collect();
    if shares.iter().filter(|s| **s > 0.0).count() < 2 {
        return shares;
    }
    // The first of the heaviest items; ties are fine, it only matters
    // that the choice is deterministic.
    let (heaviest, s_max) =
        shares.iter().copied().enumerate().fold(
            (0, 0.0),
            |best, (i, s)| if s > best.1 { (i, s) } else { best },
        );

    let solve = |v_max: f64| -> Vec<f64> {
        let c = v_max * (1.0 - v_max) / s_max;
        shares
            .iter()
            .enumerate()
            .map(|(j, s)| {
                if j == heaviest {
                    v_max
                } else {
                    (1.0 - (1.0 - 4.0 * c * s).max(0.0).sqrt()) / 2.0
                }
            })
            .collect()
    };
    let sum = |v: &[f64]| v.iter().sum::<f64>();

    let (mut lo, mut hi) = (0.0, 1.0 - 1e-9);
    let at_hi = solve(hi);
    if sum(&at_hi) <= 1.0 {
        // One item has half the weight or more, so no sequence without
        // repeats can give it its full share. The closest is to come back
        // to it after every other item, which is what `v_max` ≈ 1 does.
        let s = sum(&at_hi);
        return at_hi.iter().map(|x| x / s).collect();
    }
    // 64 halvings exhaust f64 precision on [0, 1].
    for _ in 0..64 {
        let mid = (lo + hi) / 2.0;
        if sum(&solve(mid)) < 1.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let v = solve(hi);
    let s = sum(&v);
    v.iter().map(|x| x / s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The long-run share of each item under the no-repeat chain built
    /// from `v`: proportional to `v (1 - v)`.
    fn stationary(v: &[f64]) -> Vec<f64> {
        let raw: Vec<f64> = v.iter().map(|x| x * (1.0 - x)).collect();
        let s: f64 = raw.iter().sum();
        raw.iter().map(|x| x / s).collect()
    }

    #[test]
    fn adjusted_shares_reproduce_the_weights() {
        for weights in [
            vec![1, 1],
            vec![1, 1, 1],
            vec![1, 2, 3, 4],
            vec![4, 1, 1, 1, 1, 1],
            vec![49, 50, 2],
            vec![3, 3, 0, 4],
        ] {
            let v = adjusted_shares(&weights);
            let total: u64 = weights.iter().sum();
            for (got, w) in stationary(&v).iter().zip(&weights) {
                let want = *w as f64 / total as f64;
                assert!((got - want).abs() < 1e-9, "{weights:?}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn a_dominant_weight_does_not_break_anything() {
        let v = adjusted_shares(&[10, 1, 1]);
        // The heavy item comes back after every other item.
        assert!(v[0] > 0.999_999, "{v:?}");
        assert!((v.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(v.iter().all(|x| x.is_finite() && *x >= 0.0));
        assert_eq!(adjusted_shares(&[]), Vec::<f64>::new());
        assert_eq!(adjusted_shares(&[0, 7]), vec![0.0, 1.0]);
    }

    #[test]
    fn unit_interval_bounds() {
        assert_eq!(unit_interval(0), 0.0);
        assert!(unit_interval(u64::MAX) < 1.0);
    }
}
