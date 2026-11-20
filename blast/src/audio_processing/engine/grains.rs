//! Granular playback (VOXc): overlap-add of two Hann-windowed grains, so a
//! voice can change its pitch without its speed (`pitch=`) or its speed
//! without its pitch (`stretch=`). The state is a fixed pair of grains; the
//! window is computed, so nothing here allocates.

use crate::audio_processing::blast_time::sample_rate;

/// The grain length (40 ms, or the nearest even frame count).
const GRAIN_SECS: f64 = 0.040;

/// How far (each way) a new grain may be moved from its nominal start to line
/// up with the outgoing one (1.5 ms).
const SEARCH_SECS: f64 = 0.0015;
/// The span the two are compared over (about 5.3 ms), in `TAPS` samples.
const MATCH_SECS: f64 = 0.0053;
/// Comparison samples; fixed so that the search needs no allocation.
pub(crate) const TAPS: usize = 64;

/// The size of the alignment search, in frames.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Search {
    /// Candidates run from -reach to +reach, a frame apart.
    pub reach: i32,
    /// Frames between comparison samples.
    pub dec: f64,
}

/// One grain: where it reads, how far into its window it is, and which way
/// it reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Grain {
    pub pos: f64,
    pub age: u32,
    /// +1 or -1.
    pub dir: f64,
}

/// Two grains half a window apart, whose Hann windows sum to 1.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GrainSet {
    pub grains: [Grain; 2],
    /// The grain length in frames (even).
    len: u32,
    // frames since the last grain started
    since: u32,
    search: Search,
}

impl GrainSet {
    /// Grains of 40 ms at the current sample rate (48 kHz when it isn't
    /// known yet).
    pub fn new() -> Self {
        let sr = match sample_rate::get() {
            0 => 48_000.0,
            n => f64::from(n),
        };
        let half = (GRAIN_SECS * sr / 2.0).round().max(1.0) as u32;
        let idle = Grain {
            pos: 0.0,
            age: 0,
            dir: 1.0,
        };
        Self {
            grains: [idle; 2],
            len: half * 2,
            since: 0,
            search: Search {
                reach: (SEARCH_SECS * sr).round().max(1.0) as i32,
                dec: (MATCH_SECS * sr / TAPS as f64).round().max(1.0),
            },
        }
    }

    /// Frames between grain starts: half the grain.
    pub fn hop(&self) -> u32 {
        self.len / 2
    }

    /// `0.5 - 0.5cos` over the grain; 0 once a grain is over.
    pub fn window(&self, age: u32) -> f32 {
        if age >= self.len {
            return 0.0;
        }
        let x = std::f64::consts::TAU * f64::from(age) / f64::from(self.len);
        (0.5 - 0.5 * x.cos()) as f32
    }

    /// Starts at `pos` with the first grain fully open (a hard cut) and the
    /// second one about to fade in.
    pub fn reseed(&mut self, pos: f64, dir: f64) {
        self.grains = [
            Grain {
                pos,
                age: self.hop(),
                dir,
            },
            Grain { pos, age: 0, dir },
        ];
        self.since = 0;
    }

    /// Adds the windowed reads of both grains to `out`. `read(pos, buf)`
    /// fills `buf` (as long as `out`) with the source frame at `pos`.
    pub fn render(&self, mut read: impl FnMut(f64, &mut [f32]), out: &mut [f32]) {
        let mut buf = [0.0f32; super::voice::MAX_VOICE_CHANNELS];
        let n = out.len().min(buf.len());
        for g in &self.grains {
            let w = self.window(g.age);
            if w == 0.0 {
                continue;
            }
            read(g.pos, &mut buf[..n]);
            for (o, s) in out.iter_mut().zip(&buf[..n]) {
                *o += s * w;
            }
        }
    }

    /// One frame on: grains age and `travel` moves each (position and
    /// direction); when a hop has passed, the older grain restarts going
    /// `spawn_dir`, at the position `pick` returns. `pick` gets the grain that
    /// stays (the outgoing one) and the search size, to line the new grain up
    /// with it.
    pub fn advance(
        &mut self,
        spawn_dir: f64,
        mut travel: impl FnMut(&mut f64, &mut f64),
        pick: impl FnOnce(Grain, Search) -> f64,
    ) {
        for g in &mut self.grains {
            g.age = g.age.saturating_add(1);
            travel(&mut g.pos, &mut g.dir);
        }
        self.since += 1;
        if self.since >= self.hop() {
            self.since = 0;
            let older = usize::from(self.grains[1].age > self.grains[0].age);
            let pos = pick(self.grains[1 - older], self.search);
            self.grains[older] = Grain {
                pos,
                age: 0,
                dir: spawn_dir,
            };
        }
    }
}

/// The start for a new grain: `nominal` moved by up to `search.reach` frames
/// so that what it reads from there best matches what the outgoing grain
/// reads from `outgoing` on. Both move `step` source frames per output frame.
/// `mono(pos)` is the source at `pos` (already inside the region, looped), as
/// one channel. Normalised cross-correlation over `TAPS` samples: at most
/// `(2 * reach + 1) * TAPS` multiply-adds for the dot products and as many for
/// the energies. Ties and silence keep `nominal`.
pub(crate) fn align(
    nominal: f64,
    outgoing: f64,
    step: f64,
    search: Search,
    mut mono: impl FnMut(f64) -> f32,
) -> f64 {
    if step == 0.0 {
        return nominal;
    }
    let gap = step * search.dec;
    let mut reference = [0.0f32; TAPS];
    let mut energy = 0.0f32;
    for (i, r) in reference.iter_mut().enumerate() {
        *r = mono(outgoing + gap * i as f64);
        energy += *r * *r;
    }
    if energy < 1e-6 {
        return nominal;
    }
    let mut score = |offset: i32| -> f32 {
        let start = nominal + f64::from(offset);
        let (mut dot, mut en) = (0.0f32, 0.0f32);
        for (i, r) in reference.iter().enumerate() {
            let v = mono(start + gap * i as f64);
            dot += r * v;
            en += v * v;
        }
        match dot > 0.0 {
            true => dot / (en + 1e-9).sqrt(),
            false => 0.0,
        }
    };
    let (mut best, mut best_score) = (0, score(0));
    for off in 1..=search.reach {
        for o in [off, -off] {
            let s = score(o);
            if s > best_score * 1.0001 {
                (best, best_score) = (o, s);
            }
        }
    }
    nominal + f64::from(best)
}
