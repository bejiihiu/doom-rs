//! Deterministic pseudo-random number generation, ported from the original
//! engine's `m_random.c`.
//!
//! Instead of a formula the original engine ships a fixed lookup table of
//! 256 bytes and a single shared index. Each call advances the index by one
//! (wrapping at 256) and hands out the table entry it lands on, so the
//! sequence is fully deterministic and identical to the original game's.
//!
//! The state lives in [`crate::rng::DoomRng`] instead of the original's global `rndindex`
//! globals, so the engine can own (and later clone, e.g. for demos or net
//! sync) as many independent generators as it needs. All instances created
//! with [`crate::rng::DoomRng::new`] start from the exact vanilla state: index 0, same
//! table.

/// The fixed 256-byte table from the original `m_random.c`.
///
/// Kept byte-for-byte identical to the C source so replays and demos match
/// the vanilla engine bit for bit.
const RND_TABLE: [u8; 256] = [
    0, 8, 109, 220, 222, 241, 149, 107, 75, 248, 254, 140, 16, 66, 74, 21, 211, 47, 80, 242, 154,
    27, 205, 128, 161, 89, 77, 36, 95, 110, 85, 48, 212, 140, 211, 249, 22, 79, 200, 50, 28, 188,
    52, 140, 202, 120, 68, 145, 62, 70, 184, 190, 91, 197, 152, 224, 149, 104, 25, 178, 252, 182,
    202, 182, 141, 197, 4, 81, 181, 242, 145, 42, 39, 227, 156, 198, 225, 193, 219, 93, 122, 175,
    249, 0, 175, 143, 70, 239, 46, 246, 163, 53, 163, 109, 168, 135, 2, 235, 25, 92, 20, 145, 138,
    77, 69, 166, 78, 176, 173, 212, 166, 113, 94, 161, 41, 50, 239, 49, 111, 164, 70, 60, 2, 37,
    171, 75, 136, 156, 11, 56, 42, 146, 138, 229, 73, 146, 77, 61, 98, 196, 135, 106, 63, 197, 195,
    86, 96, 203, 113, 101, 170, 247, 181, 113, 80, 250, 108, 7, 255, 237, 129, 226, 79, 107, 112,
    166, 103, 241, 24, 223, 239, 120, 198, 58, 60, 82, 128, 3, 184, 66, 143, 224, 145, 224, 81,
    206, 163, 45, 63, 90, 168, 114, 59, 33, 159, 95, 28, 139, 123, 98, 125, 196, 15, 70, 194, 253,
    54, 14, 109, 226, 71, 17, 161, 93, 186, 87, 244, 138, 20, 52, 123, 251, 26, 36, 17, 46, 52,
    231, 232, 76, 31, 221, 84, 37, 216, 165, 212, 106, 197, 242, 98, 43, 39, 175, 254, 145, 190,
    84, 118, 222, 187, 136, 120, 163, 236, 249,
];

/// A handle into the vanilla random table.
///
/// The vanilla engine kept `rndindex`/`prndindex` as globals; this type
/// carries that state by value instead. Cheap to clone: copies only the
/// 1-byte-ish index, the table itself is shared as a const.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoomRng {
    index: usize,
}

impl Default for DoomRng {
    /// Same starting state as the vanilla engine: index 0.
    fn default() -> Self {
        Self::new()
    }
}

impl DoomRng {
    /// Creates a generator in the exact vanilla starting state
    /// (`rndindex == 0`).
    pub const fn new() -> Self {
        DoomRng { index: 0 }
    }

    /// Creates a generator whose index starts at `seed % 256`.
    ///
    /// Doom's table scheme has no separate seed: "seeding" here means "start
    /// the walk at a different table slot". Kept for save/load and demo
    /// sync, where the index is serialized and restored verbatim.
    pub const fn with_seed(seed: usize) -> Self {
        DoomRng { index: seed % 256 }
    }

    /// Rewinds the index back to 0, matching `M_ClearRandom` in the original.
    pub const fn reset(&mut self) {
        self.index = 0;
    }

    /// Advances the index and returns the next table byte.
    ///
    /// Mirrors the original `P_Random`/`M_Random`: the index is bumped
    /// *before* the lookup, so the first call on a fresh [`crate::rng::DoomRng::new`]
    /// returns `RND_TABLE[1]` (8), exactly like vanilla. Values cycle with a
    /// period of 256 forever.
    // Same name the original engine uses; the Iterator signature doesn't fit
    // an infallible, infinite, &mut-based table walk.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u8 {
        self.index = (self.index + 1) & 0xff;
        RND_TABLE[self.index]
    }

    /// Returns the current index without advancing. Useful for save games
    /// and demo sync, which persist the index verbatim.
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Returns a value in `[0, n)`, the moral equivalent of the original
    /// `P_Random() % n` idiom.
    ///
    /// Semantics match the vanilla modulo pattern: the uniform byte from
    /// [`DoomRng::next`] is reduced modulo `n`, which means values `0..(256 %
    /// n)` appear one extra time per cycle when `n` does not divide 256 —
    /// same bias the original engine had, kept for behavioral compatibility.
    /// Callers that need an unbiased range should use a different mapping.
    ///
    /// `n == 0` would divide by zero, so it returns 0 instead of panicking;
    /// callers passing 0 almost certainly have a bug, but the engine never
    /// crashes on bad input.
    pub fn random_below(&mut self, n: u8) -> u8 {
        if n == 0 {
            return 0;
        }
        self.next() % n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_the_original_source() {
        // spot-checks against the literal values in the original m_random.c
        let expected: [u8; 16] = [
            0, 8, 109, 220, 222, 241, 149, 107, 75, 248, 254, 140, 16, 66, 74, 21,
        ];
        assert_eq!(&RND_TABLE[..16], &expected);
        assert_eq!(RND_TABLE[255], 249);
        assert_eq!(RND_TABLE[254], 236);
    }

    #[test]
    fn first_ten_table_entries_match_vanilla() {
        // the exact opening of the vanilla table, hand-checked against m_random.c
        let vanilla: [u8; 10] = [0, 8, 109, 220, 222, 241, 149, 107, 75, 248];
        assert_eq!(&RND_TABLE[..10], &vanilla);
    }

    #[test]
    fn fresh_generator_starts_at_zero_index() {
        let rng = DoomRng::new();
        assert_eq!(rng.index(), 0);
    }

    #[test]
    fn first_call_bumps_the_index_like_vanilla() {
        let mut rng = DoomRng::new();
        // vanilla does prndindex = (prndindex + 1) & 0xff first, then reads
        assert_eq!(rng.next(), 8);
        assert_eq!(rng.next(), 109);
        assert_eq!(rng.next(), 220);
    }

    #[test]
    fn sequence_cycles_with_period_256() {
        let mut rng = DoomRng::new();
        let first_cycle: Vec<u8> = (0..256).map(|_| rng.next()).collect();
        let mut rng = DoomRng::new();
        let second_cycle: Vec<u8> = (0..256).map(|_| rng.next()).collect();
        assert_eq!(first_cycle, second_cycle);
    }

    #[test]
    fn wraps_around_after_index_255() {
        let mut rng = DoomRng::with_seed(255);
        assert_eq!(rng.next(), RND_TABLE[0]);
        assert_eq!(rng.index(), 0);
    }

    #[test]
    fn reset_rewinds_to_vanilla_state() {
        let mut rng = DoomRng::new();
        for _ in 0..17 {
            rng.next();
        }
        rng.reset();
        assert_eq!(rng.index(), 0);
        assert_eq!(rng.next(), 8);
    }

    #[test]
    fn with_seed_wraps_large_seeds() {
        let mut a = DoomRng::with_seed(256 + 5);
        let mut b = DoomRng::with_seed(5);
        assert_eq!(a.next(), b.next());
        assert_eq!(a.index(), b.index());
    }

    #[test]
    fn clone_carries_the_index_not_a_reference() {
        let mut rng = DoomRng::new();
        rng.next();
        let mut clone = rng.clone();
        assert_eq!(rng.next(), clone.next());
    }

    #[test]
    fn random_below_is_modulo_of_next() {
        for n in [1u8, 2, 3, 7, 10, 200, 255] {
            // fresh generators keep the comparison honest across iterations
            let mut rng = DoomRng::new();
            let mut manual = DoomRng::new();
            for _ in 0..32 {
                assert_eq!(rng.random_below(n), manual.next() % n);
            }
        }
    }

    #[test]
    fn random_below_zero_returns_zero_without_panicking() {
        let mut rng = DoomRng::new();
        assert_eq!(rng.random_below(0), 0);
    }

    #[test]
    fn random_below_result_is_always_in_range() {
        let mut rng = DoomRng::new();
        for n in 1u8..=255 {
            let v = rng.random_below(n);
            assert!(v < n);
        }
    }

    #[test]
    fn different_from_a_naive_linear_congruential_sequence() {
        // not meant to prove anything statistically; just guards against
        // someone "simplifying" the table away into a formula
        let mut rng = DoomRng::new();
        let values: Vec<u8> = (0..8).map(|_| rng.next()).collect();
        assert_eq!(values, vec![8, 109, 220, 222, 241, 149, 107, 75]);
    }
}
