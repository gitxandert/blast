//! Wavetables (WAVb): single cycles cut from a track or buffer, band-limited
//! into mip levels with an FFT, and read back with a 4-point Hermite
//! interpolator. A table is built on the command thread (`build` allocates)
//! and reaches the audio thread as an `Arc`; `sample` never allocates.
//!
//! Level `k` keeps the harmonics `1..=h(k)` of every cycle, where `h(0)` is
//! `frames / 2 - 1` and each level halves the count (never below the
//! fundamental). The reader picks the level for the frequency it plays, so no
//! partial of the level in use is past Nyquist.

use ss_port::{Backend, SimpleFft};

/// The fewest samples in a cycle.
pub const MIN_TABLE_FRAMES: usize = 256;
/// The most samples in a cycle.
pub const MAX_TABLE_FRAMES: usize = 4096;
/// The most cycles in a table.
pub const MAX_TABLE_SLICES: usize = 256;
/// The most samples (`slices * levels * frames`) a table may hold: 64 MB.
pub const MAX_TABLE_SAMPLES: usize = 16 << 20;

/// The share of Nyquist the highest kept harmonic of a level may reach.
const NYQUIST_SHARE: f64 = 0.9;
/// The top share of a level's frequency range over which it crossfades into
/// the next level.
const FADE_SHARE: f64 = 0.1;

/// A table's levels for cycles of `frames` samples: `log2(frames) - 1`.
pub fn levels_for(frames: usize) -> usize {
    (frames.max(4).ilog2() as usize).saturating_sub(1)
}

/// Whether `frames` is a cycle length a table takes.
pub fn frames_ok(frames: usize) -> bool {
    frames.is_power_of_two() && (MIN_TABLE_FRAMES..=MAX_TABLE_FRAMES).contains(&frames)
}

/// Whether a table of `slices` cycles of `frames` samples fits in memory.
pub fn fits(slices: usize, frames: usize) -> bool {
    slices
        .checked_mul(levels_for(frames))
        .and_then(|n| n.checked_mul(frames))
        .is_some_and(|n| n <= MAX_TABLE_SAMPLES)
}

// the harmonics level `k` keeps
fn harmonics(frames: usize, k: usize) -> usize {
    ((frames / 2) >> k).saturating_sub(1).max(1)
}

/// Single cycles, band-limited per level. Immutable once built.
#[derive(Debug)]
pub struct WaveTable {
    frames: usize,
    slices: usize,
    levels: usize,
    // [slice][level][frame]
    data: Box<[f32]>,
}

impl WaveTable {
    /// Cycles in the table.
    pub fn slices(&self) -> usize {
        self.slices
    }

    /// Samples in a cycle.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Mip levels.
    pub fn levels(&self) -> usize {
        self.levels
    }

    /// One level of one cycle (tests).
    #[cfg(test)]
    pub(crate) fn cycle(&self, slice: usize, level: usize) -> &[f32] {
        let start = (slice * self.levels + level) * self.frames;
        &self.data[start..start + self.frames]
    }

    /// Cuts `samples` (interleaved, `channels` wide, averaged to mono) into
    /// `slices` equal parts, resamples each to one cycle of `frames` samples
    /// by linear interpolation (the last sample wraps to the cycle's own
    /// start, so the cycle closes), removes its DC and band-limits it into
    /// the mip levels. Command thread only: it allocates.
    ///
    /// `frames` should be a power of two (`frames_ok`); anything else is
    /// rounded to the nearest one that is allowed. `slices` is clamped to
    /// `1..=MAX_TABLE_SLICES`.
    pub fn build(samples: &[f32], channels: usize, slices: usize, frames: usize) -> WaveTable {
        let frames = if frames_ok(frames) {
            frames
        } else {
            frames
                .clamp(MIN_TABLE_FRAMES, MAX_TABLE_FRAMES)
                .next_power_of_two()
                .min(MAX_TABLE_FRAMES)
        };
        let slices = slices.clamp(1, MAX_TABLE_SLICES);
        let levels = levels_for(frames);
        let channels = channels.max(1);
        let mono: Vec<f32> = samples
            .chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect();

        let mut data = vec![0.0f32; slices * levels * frames];
        let mut fft = SimpleFft::new(frames, Backend::Scalar);
        let mut cycle = vec![0.0f32; frames];
        let zeros = vec![0.0f32; frames];
        let (mut xr, mut xi) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        let (mut yr, mut yi) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        let (mut or, mut oi) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        let scale = 1.0 / frames as f32;

        for s in 0..slices {
            cut_cycle(&mono, s, slices, &mut cycle);
            fft.fft(&cycle, &zeros, &mut xr, &mut xi);
            for k in 0..levels {
                let h = harmonics(frames, k);
                yr.fill(0.0);
                yi.fill(0.0);
                // bin 0 (DC) and every bin past `h` (and its mirror) stay 0
                for b in 1..=h {
                    yr[b] = xr[b];
                    yi[b] = xi[b];
                    yr[frames - b] = xr[frames - b];
                    yi[frames - b] = xi[frames - b];
                }
                fft.ifft(&yr, &yi, &mut or, &mut oi);
                let start = (s * levels + k) * frames;
                for (d, v) in data[start..start + frames].iter_mut().zip(&or) {
                    *d = v * scale;
                }
            }
        }
        WaveTable {
            frames,
            slices,
            levels,
            data: data.into_boxed_slice(),
        }
    }

    /// The level (with a fraction: the crossfade into the next one) to read
    /// when the phase moves `dt` of a cycle per output sample. Level `k` is
    /// the first whose highest harmonic stays below 90 % of Nyquist; over
    /// the top tenth of the range it is allowed, it fades into level `k + 1`.
    pub fn level_for(&self, dt: f64) -> f32 {
        for k in 0..self.levels.saturating_sub(1) {
            let limit = 0.5 * NYQUIST_SHARE / harmonics(self.frames, k) as f64;
            if dt <= limit {
                let from = limit * (1.0 - FADE_SHARE);
                let fade = if dt > from {
                    (dt - from) / (limit - from)
                } else {
                    0.0
                };
                return k as f32 + fade as f32;
            }
        }
        self.levels.saturating_sub(1) as f32
    }

    /// One sample: `slice_pos` in `0..=slices - 1` (a linear crossfade
    /// between the two cycles around it), `level_f` a level with a fraction
    /// (crossfaded the same way), `phase` in `0..1` over the cycle. Never
    /// allocates.
    pub fn sample(&self, slice_pos: f32, level_f: f32, phase: f64) -> f32 {
        let top = (self.slices - 1) as f32;
        let slice_pos = if slice_pos.is_finite() {
            slice_pos.clamp(0.0, top)
        } else {
            0.0
        };
        let s0 = slice_pos as usize;
        let sf = slice_pos - s0 as f32;
        let s1 = (s0 + 1).min(self.slices - 1);
        let level_f = if level_f.is_finite() {
            level_f.clamp(0.0, (self.levels - 1) as f32)
        } else {
            0.0
        };
        let l0 = level_f as usize;
        let lf = level_f - l0 as f32;
        let l1 = (l0 + 1).min(self.levels - 1);

        let at = |s: usize, l: usize| self.hermite(s, l, phase);
        let along = |s: usize| match lf {
            0.0 => at(s, l0),
            _ => {
                let a = at(s, l0);
                a + (at(s, l1) - a) * lf
            }
        };
        match sf {
            0.0 => along(s0),
            _ => {
                let a = along(s0);
                a + (along(s1) - a) * sf
            }
        }
    }

    // 4-point Hermite over one cycle, wrapping
    fn hermite(&self, slice: usize, level: usize, phase: f64) -> f32 {
        let start = (slice * self.levels + level) * self.frames;
        let cycle = &self.data[start..start + self.frames];
        let mask = self.frames - 1;
        let x = phase.rem_euclid(1.0) * self.frames as f64;
        let i = x as usize;
        let t = (x - i as f64) as f32;
        let (y0, y1, y2, y3) = (
            cycle[i.wrapping_sub(1) & mask],
            cycle[i & mask],
            cycle[(i + 1) & mask],
            cycle[(i + 2) & mask],
        );
        let c0 = y1;
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        ((c3 * t + c2) * t + c1) * t + c0
    }
}

// slice `s` of `slices` of `mono`, resampled to `out.len()` samples with its
// DC removed
fn cut_cycle(mono: &[f32], s: usize, slices: usize, out: &mut [f32]) {
    let len = mono.len();
    let start = s * len / slices;
    let end = ((s + 1) * len / slices).max(start + 1).min(len);
    let span = &mono[start.min(len.saturating_sub(1))..end.max(1).min(len)];
    if span.is_empty() {
        out.fill(0.0);
        return;
    }
    let m = span.len();
    let step = m as f64 / out.len() as f64;
    for (j, o) in out.iter_mut().enumerate() {
        let x = j as f64 * step;
        let i = x as usize;
        let t = (x - i as f64) as f32;
        let (a, b) = (span[i], span[(i + 1) % m]);
        *o = a + (b - a) * t;
    }
    let mean = out.iter().sum::<f32>() / out.len() as f32;
    for o in out.iter_mut() {
        *o -= mean;
    }
}
