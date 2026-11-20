// Xoroshiro128Plus
// muy good and fast
//
pub struct X128P {
    s0: u64,
    s1: u64,
}

// expands a 64-bit seed into 128 bits of state, and mixes `fast_seed`
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

impl X128P {
    pub fn new(seed: u64) -> Self {
        let s0 = splitmix64(seed);
        let s1 = splitmix64(seed.wrapping_add(0x9E3779B97F4A7C15));

        Self { s0, s1 }
    }

    #[inline]
    fn rotl(x: u64, k: u32) -> u64 {
        x.rotate_left(k)
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.s0.wrapping_add(self.s1);

        let s1 = self.s1 ^ self.s0;
        self.s0 = Self::rotl(self.s0, 55) ^ s1 ^ (s1 << 14);
        self.s1 = Self::rotl(s1, 36);

        result
    }

    pub fn next_f64(&mut self) -> f64 {
        // Scale into [0, 1)
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    pub fn next_f32(&mut self) -> f32 {
        self.next_f64() as f32
    }

    pub fn next_i64_range(&mut self, lower: i64, upper: i64) -> i64 {
        let r = self.next_u64();
        let range: u64 = match upper > lower {
            true => (upper - lower).try_into().unwrap(),
            false => (lower - upper).try_into().unwrap(),
        };

        let val = ((r as u128 * range as u128) >> 64) as i64;
        lower + val
    }
}

thread_local! {
    // set by `set_session_seed`: (seed, calls so far)
    static SESSION: std::cell::Cell<Option<(u64, u64)>> = const { std::cell::Cell::new(None) };
}

/// Makes `fast_seed` deterministic on this thread: from now on it answers
/// `splitmix64` of the seed and a call counter (`--export --seed N`). Per
/// thread, so renders running side by side (tests) can't disturb each other;
/// a render is single-threaded, and the command thread is where objects are
/// built.
pub fn set_session_seed(seed: u64) {
    SESSION.with(|s| s.set(Some((seed, 0))));
}

/// Back to clock seeds on this thread.
pub fn clear_session_seed() {
    SESSION.with(|s| s.set(None));
}

/// A fresh seed for each call: the clock's nanoseconds XOR a counter, mixed
/// through splitmix64. Portable, no `unsafe`; not for cryptography. After
/// `set_session_seed`, a deterministic sequence instead.
pub fn fast_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static CTR: AtomicU64 = AtomicU64::new(1);

    if let Some((seed, n)) = SESSION.with(std::cell::Cell::get) {
        SESSION.with(|s| s.set(Some((seed, n + 1))));
        return splitmix64(splitmix64(seed) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let count = CTR.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    splitmix64(nanos ^ count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_seed_differs_between_calls() {
        let seeds: Vec<u64> = (0..64).map(|_| fast_seed()).collect();
        for (i, a) in seeds.iter().enumerate() {
            assert!(!seeds[i + 1..].contains(a), "repeated seed {a}");
        }
    }

    #[test]
    fn session_seed_makes_the_sequence_repeat() {
        set_session_seed(5);
        let a: Vec<u64> = (0..8).map(|_| fast_seed()).collect();
        set_session_seed(5);
        let b: Vec<u64> = (0..8).map(|_| fast_seed()).collect();
        set_session_seed(6);
        let c: Vec<u64> = (0..8).map(|_| fast_seed()).collect();
        clear_session_seed();
        assert_eq!(a, b);
        assert_ne!(a, c);
        for (i, x) in a.iter().enumerate() {
            assert!(!a[i + 1..].contains(x));
        }
    }
}
