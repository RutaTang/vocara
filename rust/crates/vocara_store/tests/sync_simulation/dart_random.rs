//! The Dart VM's `Random(seed)` (`sdk/lib/_internal/vm/lib/math_patch.dart`),
//! bit for bit, so a simulation seed draws the same decision sequence as the
//! Dart episode of that seed (the same algorithm the ported DSP and mixer
//! tests use for their Dart-pinned fixtures).
//!
//! The algorithm: the seed is scrambled by Thomas Wang's 64-bit mix (0 maps
//! to 0x5a17), the generator is cranked four times "to distribute the seed
//! bits a bit further", and each step is a multiply-with-carry over base
//! 2^32 (`state = A * lo32(state) + hi32(state)`, A = 0xffffda61).
//! `nextInt(max)` takes the low 32 bits: masked for a power of two,
//! otherwise reduced modulo `max` with the biased tail rejected.

/// Dart's `Random(seed)` on the VM.
pub struct DartRandom {
    state: u64,
}

impl DartRandom {
    /// `Random(seed)`.
    pub fn new(seed: i64) -> Self {
        // `_Random._setupSeed`: Dart ints wrap at 64 bits and `>>>` is the
        // logical shift, which is u64 arithmetic.
        let mut n = seed as u64;
        n = (!n).wrapping_add(n << 21);
        n ^= n >> 24;
        n = n.wrapping_mul(265);
        n ^= n >> 14;
        n = n.wrapping_mul(21);
        n ^= n >> 28;
        n = n.wrapping_add(n << 31);
        if n == 0 {
            n = 0x5a17;
        }
        let mut r = DartRandom { state: n };
        for _ in 0..4 {
            r.next_state();
        }
        r
    }

    fn next_state(&mut self) {
        const A: u64 = 0xffff_da61;
        let lo = self.state & 0xFFFF_FFFF;
        let hi = self.state >> 32;
        // A * lo + hi < 2^64 for 32-bit lo and hi: it never wraps.
        self.state = A * lo + hi;
    }

    /// `nextInt(max)`: uniform in `0..max` (`0 < max <= 2^32`).
    pub fn next_int(&mut self, max: usize) -> usize {
        let max = u64::try_from(max).expect("max fits in 64 bits");
        assert!(
            max > 0 && max <= 1 << 32,
            "RangeError: max must be positive and <= 2^32, was {max}"
        );
        if max.is_power_of_two() {
            self.next_state();
            return usize::try_from(self.state & 0xFFFF_FFFF & (max - 1)).expect("below max");
        }
        loop {
            self.next_state();
            let rnd32 = self.state & 0xFFFF_FFFF;
            let result = rnd32 % max;
            if rnd32 - result + max <= 1 << 32 {
                return usize::try_from(result).expect("below max");
            }
        }
    }

    /// `nextInt(max)` as an `i64` (the episode's millisecond and position
    /// draws).
    pub fn next_i64(&mut self, max: usize) -> i64 {
        i64::try_from(self.next_int(max)).expect("below max")
    }
}

#[test]
fn a_seed_replays_the_same_sequence_and_seeds_differ() {
    let draw = |seed| {
        let mut r = DartRandom::new(seed);
        (0..32).map(|_| r.next_int(1000)).collect::<Vec<_>>()
    };
    assert_eq!(draw(0x5EED_0001), draw(0x5EED_0001));
    assert_ne!(draw(0x5EED_0001), draw(0x5EED_0002));
    // Seed 0 does not start from the degenerate all-zero state.
    assert!(draw(0).iter().any(|&v| v != 0));
}

#[test]
fn next_int_stays_in_range_for_powers_of_two_and_the_rest() {
    let mut r = DartRandom::new(42);
    for max in [
        1,
        2,
        3,
        5,
        8,
        10,
        134,
        2400,
        1 << 31,
        (1 << 32) - 1,
        1 << 32,
    ] {
        for _ in 0..200 {
            assert!(r.next_int(max) < max);
        }
    }
}
