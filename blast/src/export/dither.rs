//! Float to integer sample conversion with TPDF dither (writer thread only).

use crate::audio_processing::blast_rand::X128P;

/// Converts f32 samples in [-1, 1] to signed integers of a given bit depth:
/// clamp, add triangular dither of +-1 LSB peak, round, clamp to range.
pub struct Quantizer {
    scale: f32,
    min: i32,
    max: i32,
    rng: X128P,
}

impl Quantizer {
    /// A quantizer for `bits` (16 or 24) with a given dither seed.
    pub fn new(bits: u32, seed: u64) -> Self {
        let scale = (1u32 << (bits - 1)) as f32;
        Self {
            scale,
            min: -(scale as i32),
            max: scale as i32 - 1,
            rng: X128P::new(seed),
        }
    }

    /// One sample. NaN becomes 0 and overs clamp to the integer range.
    pub fn quantize(&mut self, x: f32) -> i32 {
        if x.is_nan() {
            return 0;
        }
        let dither = self.rng.next_f32() - self.rng.next_f32();
        let v = x.clamp(-1.0, 1.0) * self.scale + dither;
        (v.round() as i32).clamp(self.min, self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_stays_within_one_lsb() {
        let mut src = X128P::new(7);
        let mut q = Quantizer::new(16, 1);
        for _ in 0..10_000 {
            let x = src.next_f32() * 2.0 - 1.0;
            let out = q.quantize(x) as f32;
            assert!((out - x * q.scale).abs() <= 1.5, "x={x} out={out}");
        }
    }

    #[test]
    fn dither_is_triangular() {
        let mut q = Quantizer::new(16, 42);
        let n = 100_000;
        let (mut sum, mut zeros) = (0i64, 0usize);
        for _ in 0..n {
            let v = q.quantize(0.0);
            assert!((-1..=1).contains(&v), "got {v}");
            sum += i64::from(v);
            zeros += usize::from(v == 0);
        }
        assert!((sum as f64 / n as f64).abs() < 0.02);
        // Triangular density 1-|d| on (-1,1): P(|d| < 0.5) = 0.75.
        let p0 = zeros as f64 / n as f64;
        assert!((p0 - 0.75).abs() < 0.05, "P(0) = {p0}");
    }

    #[test]
    fn clamps_overs_and_nan() {
        let mut q = Quantizer::new(16, 3);
        assert_eq!(q.quantize(2.0), 32767);
        assert_eq!(q.quantize(-2.0), -32768);
        assert_eq!(q.quantize(f32::NAN), 0);
        let mut q24 = Quantizer::new(24, 3);
        assert_eq!(q24.quantize(2.0), 8_388_607);
        assert_eq!(q24.quantize(-2.0), -8_388_608);
    }
}
