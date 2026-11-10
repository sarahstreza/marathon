//! A tiny deterministic PRNG for shuffles.
//!
//! This is SplitMix64 (Steele, Lea & Flood, 2014) with Lemire's unbiased
//! bounded sampling. It is implemented here rather than taken from `rand`
//! for two reasons:
//!
//! - **Stability.** Shuffle orders are derived from seeds stored in
//!   cursors in the database. The algorithm must never change underneath
//!   them, and a dependency upgrade (for example `rand`'s `shuffle` or
//!   `gen_range` changing implementation) could silently change it.
//! - **Purity is easy to review.** The crate has no RNG dependency at all, so
//!   there is no way to reach a global or OS-seeded RNG.
//!
//! Statistical quality is ample for shuffling playlists; this is not for
//! cryptography.

const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64's output function: a strong 64-bit mixer.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic random stream identified by `(seed, domain, stream)`.
#[derive(Debug, Clone)]
pub(crate) struct Rng {
    state: u64,
}

impl Rng {
    /// Creates the stream for one shuffle.
    ///
    /// `domain` separates unrelated uses (item shuffles, group shuffles…) and
    /// `stream` numbers successive shuffles within a domain (the cycle), so
    /// every shuffle a cursor ever does gets independent randomness from a
    /// single stored seed.
    pub(crate) fn new(seed: u64, domain: u64, stream: u64) -> Self {
        Self {
            state: seed ^ mix(domain.wrapping_mul(GOLDEN_GAMMA) ^ mix(stream)),
        }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN_GAMMA);
        mix(self.state)
    }

    /// Uniform integer in `0..n`. Returns 0 when `n` is 0.
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        // Lemire, "Fast Random Integer Generation in an Interval" (2019).
        let mut m = u128::from(self.next_u64()) * u128::from(n);
        if (m as u64) < n {
            let threshold = n.wrapping_neg() % n;
            while (m as u64) < threshold {
                m = u128::from(self.next_u64()) * u128::from(n);
            }
        }
        (m >> 64) as u64
    }

    /// Uniform index in `0..len`. Returns 0 when `len` is 0.
    pub(crate) fn index(&mut self, len: usize) -> usize {
        // `usize` is at most 64 bits on every supported target, and the
        // result is below `len`, so neither conversion can lose information.
        self.below(len as u64) as usize
    }

    /// Fisher–Yates shuffle in place.
    pub(crate) fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.index(i + 1);
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_same_stream() {
        let a: Vec<_> = (0..8)
            .map({
                let mut r = Rng::new(42, 1, 7);
                move |_| r.next_u64()
            })
            .collect();
        let b: Vec<_> = (0..8)
            .map({
                let mut r = Rng::new(42, 1, 7);
                move |_| r.next_u64()
            })
            .collect();
        assert_eq!(a, b);
    }

    #[test]
    fn streams_and_domains_differ() {
        let first = |seed, domain, stream| Rng::new(seed, domain, stream).next_u64();
        let base = first(42, 1, 0);
        assert_ne!(base, first(42, 1, 1));
        assert_ne!(base, first(42, 2, 0));
        assert_ne!(base, first(43, 1, 0));
    }

    /// Pins the algorithm: if this changes, every stored shuffle cursor
    /// would continue with a different order after an upgrade.
    #[test]
    fn output_is_pinned() {
        let mut r = Rng::new(0, 0, 0);
        let mut v: Vec<u32> = (0..10).collect();
        r.shuffle(&mut v);
        assert_eq!(v, PINNED);
    }
    const PINNED: [u32; 10] = [4, 9, 2, 5, 1, 7, 6, 0, 3, 8];

    #[test]
    fn below_stays_in_range() {
        let mut r = Rng::new(1, 2, 3);
        for n in 1..200u64 {
            for _ in 0..20 {
                assert!(r.below(n) < n);
            }
        }
        assert_eq!(r.below(0), 0);
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut r = Rng::new(9, 9, 9);
        let mut v: Vec<u32> = (0..100).collect();
        r.shuffle(&mut v);
        let mut sorted = v.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..100).collect::<Vec<_>>());
        assert_ne!(v, sorted, "100 elements should not stay in order");
    }

    #[test]
    fn shuffle_of_tiny_slices() {
        let mut r = Rng::new(0, 0, 0);
        let mut empty: [u8; 0] = [];
        r.shuffle(&mut empty);
        let mut one = [7];
        r.shuffle(&mut one);
        assert_eq!(one, [7]);
    }
}
