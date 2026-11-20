//! Spectral pitch shifting per choir instance (REQ-099, PSHa).
//!
//! Each choir instance can be played by a Signalsmith Stretch shifter instead
//! of the WSOLA grains. The shifter is fed the source along the instance's
//! read path (loops, ping-pong bounces and region edges follow the voice's
//! own rules) and is asked to consume `|rate| * stretch` input frames per
//! output frame while transposing by `|rate| * pitch ratio`. It renders in
//! blocks of `BLOCK` frames into a preallocated ring that the per-frame
//! engine drains one frame at a time.
//!
//! The design follows the research prototype `ShiftVoice`
//! (pitch-shift-in-rust, `candidates/blast-voice`, commit 6e4bd0c), the
//! user's own code, with the copied `travel`/`Bounds` replaced by the voice's
//! own through `ReadPath`. The crate it drives is a port of Signalsmith
//! Stretch 1.4.0 (MIT, Geraint Luff / Signalsmith Audio Ltd.).
//!
//! Nothing here allocates after `VoiceShifters::new`.

use ss_port::SignalsmithStretch;

use super::voice::{MAX_CHOIR, MAX_VOICE_CHANNELS};

/// Output frames per block: how often the shifter runs, and how late a
/// parameter change is heard (at most this many frames).
const BLOCK: usize = 64;
/// The largest `|rate| * stretch` (input frames per output frame) the
/// scratch is sized for; faster requests are clamped.
const MAX_SPEED: f64 = 4.0;
/// The transpose factor stays in this range: at rate 0 it would be a zero
/// frequency multiplier, and far above the top the output is only noise.
const MIN_TRANSPOSE: f32 = 1.0 / 32.0;
const MAX_TRANSPOSE: f32 = 64.0;

/// How a voice's source is read: a feed cursor that moves one frame at a
/// time along the same path the instance's head takes. Implemented in
/// `voice.rs`, where the loop, the region and the buffer half live.
pub(crate) trait ReadPath {
    /// Channel `ch` of source frame `frame` (silence outside the source).
    fn sample(&self, frame: usize, ch: usize) -> f32;
    /// Whether `pos` sits on a playable frame; elsewhere the feed is silent.
    fn playing(&self, pos: f64) -> bool;
    /// Moves `pos` one frame along `dir` (`travel`); true when a ping-pong
    /// loop bounced, so the caller flips the direction.
    fn step(&self, pos: &mut f64, dir: f64) -> bool;
}

/// What the shifter needs from the voice's parameters, as effective values
/// for one instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShiftParams {
    /// `|rate| * stretch`: input frames per output frame.
    pub speed: f64,
    /// `|rate| * pitch ratio`: the frequency multiplier.
    pub transpose: f32,
    /// +1, or -1 when the rate is negative: the direction the feed reads.
    pub rate_sign: f64,
}

impl ShiftParams {
    fn speed(&self) -> f64 {
        self.speed.clamp(0.0, MAX_SPEED)
    }

    fn transpose(&self) -> f32 {
        self.transpose.clamp(MIN_TRANSPOSE, MAX_TRANSPOSE)
    }
}

/// One choir instance's shifter.
pub(crate) struct ShiftVoice {
    ss: SignalsmithStretch,
    channels: usize,
    tonality_limit: f32,
    // the feed cursor, which runs ahead of the instance's head, and its
    // ping-pong direction (the sign of the rate multiplies it)
    feed: f64,
    pp_dir: f64,
    // fractional input frames owed to the shifter
    acc: f64,
    // planar input `[ch][in_cap]`
    in_buf: Vec<f32>,
    in_cap: usize,
    // planar shifter output `[ch][BLOCK]`
    out_buf: Vec<f32>,
    // interleaved output `[BLOCK][ch]`, and the frame the engine is on
    ring: Vec<f32>,
    ring_pos: usize,
}

impl ShiftVoice {
    /// `window`-frame block, a quarter of it as interval, split computation. Allocates, and then
    /// touches every page it allocated (command thread), so the audio
    /// thread never takes a first-touch page fault in a hit or a block.
    fn new(channels: usize, window: usize, sample_rate: f32, seed: u64) -> Self {
        let mut sv = Self::unfaulted(channels, window, sample_rate, seed);
        sv.prefault();
        sv
    }

    // runs the shifter once over silence (an `output_seek` and a block),
    // fills the buffers, then returns to the state `unfaulted` has
    fn prefault(&mut self) {
        let len = self.ss.output_seek_length(1.0).min(self.in_cap);
        self.in_buf.fill(0.0);
        self.out_buf.fill(0.0);
        self.ring.fill(0.0);
        let mut ins: [&[f32]; MAX_VOICE_CHANNELS] = [&[]; MAX_VOICE_CHANNELS];
        for (c, chunk) in self
            .in_buf
            .chunks(self.in_cap)
            .take(self.channels)
            .enumerate()
        {
            ins[c] = &chunk[..len];
        }
        self.ss.output_seek(&ins[..self.channels], len);
        let mut outs: [&mut [f32]; MAX_VOICE_CHANNELS] = Default::default();
        for (c, chunk) in self
            .out_buf
            .chunks_mut(BLOCK)
            .take(self.channels)
            .enumerate()
        {
            outs[c] = chunk;
        }
        let n = self.in_cap.min(BLOCK);
        self.ss.process(
            &ins[..self.channels],
            n.min(len),
            &mut outs[..self.channels],
            BLOCK,
        );
        self.ss.reset();
        self.feed = 0.0;
        self.pp_dir = 1.0;
        self.acc = 0.0;
        self.ring_pos = 0;
    }

    fn unfaulted(channels: usize, win: usize, sample_rate: f32, seed: u64) -> Self {
        let mut ss = SignalsmithStretch::new(seed);
        ss.configure(channels, win, win / 4, true);
        let seek_max = ss.output_seek_length(MAX_SPEED as f32);
        let in_cap = seek_max.max((MAX_SPEED * BLOCK as f64).ceil() as usize + 1);
        Self {
            ss,
            channels,
            tonality_limit: 8000.0 / sample_rate,
            feed: 0.0,
            pp_dir: 1.0,
            acc: 0.0,
            in_buf: vec![0.0; channels * in_cap],
            in_cap,
            out_buf: vec![0.0; channels * BLOCK],
            ring: vec![0.0; channels * BLOCK],
            ring_pos: 0,
        }
    }

    /// A hit, or an entry into the shifter: the output starts at `pos`
    /// (the instance's head), read in direction `dir` (+1 or -1, including
    /// a ping-pong bounce). A hard cut: `output_seek` resets the shifter and
    /// pre-rolls it so that output frame 0 is the onset, and the first block
    /// is rendered now, so `frame()` is already that first frame.
    pub(crate) fn hit(&mut self, src: &dyn ReadPath, pos: f64, dir: f64, p: ShiftParams) {
        self.feed = pos;
        self.pp_dir = if dir * p.rate_sign >= 0.0 { 1.0 } else { -1.0 };
        self.acc = 0.0;
        self.set_transpose(&p);
        let len = self
            .ss
            .output_seek_length(p.speed() as f32)
            .min(self.in_cap);
        self.gather(src, len, p.rate_sign);
        let mut ins: [&[f32]; MAX_VOICE_CHANNELS] = [&[]; MAX_VOICE_CHANNELS];
        for (c, chunk) in self
            .in_buf
            .chunks(self.in_cap)
            .take(self.channels)
            .enumerate()
        {
            ins[c] = &chunk[..len];
        }
        self.ss.output_seek(&ins[..self.channels], len);
        self.refill(src, p);
    }

    /// A hit from a snapshot: becomes exactly what `hit` left `snap` as, by
    /// copying (no allocation, no `output_seek`): the shifter's state, the
    /// first block's ring, the feed cursor, its ping-pong direction and the
    /// owed fraction. The snapshot must have the same shape.
    pub(crate) fn hit_from(&mut self, snap: &HitSnapshot) {
        let s = &snap.sv;
        debug_assert!(s.channels == self.channels && s.ring.len() == self.ring.len());
        self.ss.copy_state_from(&s.ss);
        self.feed = s.feed;
        self.pp_dir = s.pp_dir;
        self.acc = s.acc;
        self.ring.copy_from_slice(&s.ring);
        self.ring_pos = s.ring_pos;
    }

    /// The current output frame (`channels` samples).
    pub(crate) fn frame(&self) -> &[f32] {
        let ch = self.channels;
        &self.ring[self.ring_pos * ch..(self.ring_pos + 1) * ch]
    }

    /// Moves one output frame on; every `BLOCK` frames the next block is
    /// rendered. Parameters are sampled then, so a change is heard up to
    /// `BLOCK` frames late.
    pub(crate) fn advance(&mut self, src: &dyn ReadPath, p: ShiftParams) {
        self.ring_pos += 1;
        if self.ring_pos >= BLOCK {
            self.refill(src, p);
        }
    }

    fn set_transpose(&mut self, p: &ShiftParams) {
        self.ss
            .set_transpose_factor(p.transpose(), self.tonality_limit);
    }

    // gathers `n` frames along the read path into `in_buf` (planar) and
    // moves the feed cursor past them; frames outside the region read as
    // silence, as the head's `playing` is false there
    fn gather(&mut self, src: &dyn ReadPath, n: usize, rate_sign: f64) {
        for i in 0..n {
            if src.playing(self.feed) {
                let frame = self.feed as usize;
                for c in 0..self.channels {
                    self.in_buf[c * self.in_cap + i] = src.sample(frame, c);
                }
            } else {
                for c in 0..self.channels {
                    self.in_buf[c * self.in_cap + i] = 0.0;
                }
            }
            if src.step(&mut self.feed, rate_sign * self.pp_dir) {
                self.pp_dir = -self.pp_dir;
            }
        }
    }

    // renders the next BLOCK output frames into the ring
    fn refill(&mut self, src: &dyn ReadPath, p: ShiftParams) {
        self.acc += p.speed() * BLOCK as f64;
        let n = (self.acc.floor() as usize).min(self.in_cap);
        self.acc -= n as f64;
        self.set_transpose(&p);
        self.gather(src, n, p.rate_sign);

        let mut ins: [&[f32]; MAX_VOICE_CHANNELS] = [&[]; MAX_VOICE_CHANNELS];
        for (c, chunk) in self
            .in_buf
            .chunks(self.in_cap)
            .take(self.channels)
            .enumerate()
        {
            ins[c] = &chunk[..n];
        }
        let mut outs: [&mut [f32]; MAX_VOICE_CHANNELS] = Default::default();
        for (c, chunk) in self
            .out_buf
            .chunks_mut(BLOCK)
            .take(self.channels)
            .enumerate()
        {
            outs[c] = chunk;
        }
        self.ss
            .process(&ins[..self.channels], n, &mut outs[..self.channels], BLOCK);
        for f in 0..BLOCK {
            for c in 0..self.channels {
                self.ring[f * self.channels + c] = self.out_buf[c * BLOCK + f];
            }
        }
        self.ring_pos = 0;
    }
}

/// Everything a hit's result depends on, besides the fixed configuration:
/// which source and buffer half, where the path starts and which way it
/// goes, the region and loop, and the exact transpose factor and speed.
/// Equal keys give bit-identical hits. (REQ-099 section 4; the design is the
/// prototype's `HitKey`.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct HitKey {
    /// The source's address: an `Arc`'s pointer, as a number.
    pub source_id: usize,
    /// The buffer half that is heard (0 for a track).
    pub front_half: usize,
    /// The frame the hit reads from (the bits of the `f64` position).
    pub start: u64,
    /// The head's direction, +1 or -1.
    pub dir: i8,
    /// The sign of the rate, +1 or -1. (Not in the plan's list: `dir`
    /// is the product of this and the ping-pong direction, and the feed
    /// reads by the rate's sign alone.)
    pub rate_sign: i8,
    /// Region start and end, loop start and end.
    pub bounds: (u64, u64, u64, u64),
    /// The loop mode, with bit 7 set when a loop applies.
    pub loop_mode: u8,
    pub transpose_bits: u32,
    pub speed_bits: u32,
}

impl HitKey {
    pub(crate) fn new(
        source_id: usize,
        front_half: usize,
        (pos, dir): (f64, f64),
        bounds: (i64, i64, i64, i64),
        loop_mode: u8,
        p: &ShiftParams,
    ) -> Self {
        Self {
            source_id,
            front_half,
            start: pos.to_bits(),
            dir: if dir >= 0.0 { 1 } else { -1 },
            rate_sign: if p.rate_sign >= 0.0 { 1 } else { -1 },
            bounds: (
                bounds.0 as u64,
                bounds.1 as u64,
                bounds.2 as u64,
                bounds.3 as u64,
            ),
            loop_mode,
            transpose_bits: p.transpose().to_bits(),
            speed_bits: (p.speed() as f32).to_bits(),
        }
    }
}

/// A shifter as a hit left it (after `output_seek` and the first block), and
/// the key of that hit. Any instance with the same key becomes a copy of it.
pub(crate) struct HitSnapshot {
    sv: ShiftVoice,
    key: Option<HitKey>,
}

/// How much fresh `output_seek` work one callback may do. A cold seek costs
/// `2 + channels` units at the default window (`ColdBudget::cost` scales it
/// with `window=`), roughly in proportion to its measured time on the
/// dev machine (AMD Ryzen/AVX2 build of the pinned crate; pitch-shift-in-rust
/// `results/blast-voice/seek_cost_window.csv`, 90 ms window: about 870, 1220
/// and 3220 us median for 1, 2 and 8 channels, 128-frame callbacks being
/// 2667 us). Each callback gets `UNITS_PER_128 * frames / 128` units, at least
/// `MIN_UNITS`: one stereo cold seek per 128-frame callback. The first cold
/// seek of a callback is always allowed, even if it costs more than the whole
/// budget (8 channels: 10 units; any seek in a callback under 128 frames), so
/// that every shape can start; a second one must fit in what is left. An
/// 8-channel cold seek alone outlasts a 128-frame callback. A slower machine
/// needs a lower constant.
pub(crate) const UNITS_PER_128: u32 = 4;
const MIN_UNITS: u32 = 2;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ColdBudget {
    units: u32,
    full: u32,
}

impl ColdBudget {
    /// The budget of a callback of `frames` frames.
    pub(crate) fn for_callback(frames: usize) -> Self {
        let units = (UNITS_PER_128 as usize * frames / 128)
            .clamp(MIN_UNITS as usize, u32::MAX as usize) as u32;
        Self { units, full: units }
    }

    #[cfg(test)]
    pub(crate) fn units(&self) -> u32 {
        self.units
    }

    /// The cost of one cold seek on `channels` channels with a `window`-frame
    /// window: `2 + channels` units at the default window, in proportion to
    /// the window otherwise (a seek costs about as much as the window is
    /// long; `seek_cost_window.csv`), at least 1.
    pub(crate) fn cost(channels: usize, window: usize, sample_rate: f32) -> u32 {
        let base = default_window(sample_rate).max(1) as u64;
        let units = (2 + channels as u64) * window as u64;
        ((units + base / 2) / base).clamp(1, u64::from(u32::MAX)) as u32
    }

    /// Spends the cost of one cold seek (`ColdBudget::cost`), if it is
    /// affordable. The first seek of a callback is always allowed, so that a
    /// shape costing more than the whole budget (8 channels in a 128-frame
    /// callback: 10 against 4) can still start.
    pub(crate) fn take(&mut self, cost: u32) -> bool {
        if self.units >= cost {
            self.units -= cost;
            true
        } else if self.units == self.full {
            self.units = 0;
            true
        } else {
            false
        }
    }
}

/// What became of a pending hit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HitOutcome {
    /// Copied from a snapshot that already matched.
    Warm,
    /// Needed a fresh `output_seek` (now the snapshot).
    Cold,
    /// The budget was spent: the caller plays the note on the grains.
    Refused,
}

/// The REQ-104 default window, in seconds.
pub(crate) const DEFAULT_WINDOW_S: f64 = 0.090;
/// The clamp on `window=`, in seconds.
pub(crate) const WINDOW_RANGE_S: (f64, f64) = (0.020, 0.250);

/// The default window in frames at `sample_rate`.
pub(crate) fn default_window(sample_rate: f32) -> usize {
    (DEFAULT_WINDOW_S * f64::from(sample_rate)).round() as usize
}

/// A voice's shifters: one per built choir instance. Built on the command
/// thread (it allocates) and moved to the engine by `Command::SetShifters`;
/// the engine hands the old set back as garbage.
pub struct VoiceShifters {
    channels: usize,
    window: usize,
    // `ColdBudget::cost` for this shape, fixed at build
    pub(crate) cold_cost: u32,
    voices: Vec<ShiftVoice>,
    // the last hit, for the instances that repeat it
    snapshot: HitSnapshot,
}

impl VoiceShifters {
    /// Command thread only: allocates (about half a megabyte per stereo
    /// instance). `channels` and `instances` are clamped to `1..=8`. `window`
    /// is in frames, already clamped by the caller (`WINDOW_RANGE_S`); only
    /// a floor of 16 frames is enforced here, so that the shifter can't be
    /// configured with nothing.
    pub fn new(channels: usize, instances: usize, sample_rate: f32, window: usize) -> Self {
        let window = window.max(16);
        let channels = channels.clamp(1, MAX_VOICE_CHANNELS);
        let instances = instances.clamp(1, MAX_CHOIR);
        Self {
            channels,
            window,
            cold_cost: ColdBudget::cost(channels, window, sample_rate),
            // each instance has its own noise stream
            voices: (0..instances)
                .map(|i| ShiftVoice::new(channels, window, sample_rate, 7 + i as u64))
                .collect(),
            snapshot: HitSnapshot {
                sv: ShiftVoice::new(channels, window, sample_rate, 7),
                key: None,
            },
        }
    }

    /// Channels each shifter processes.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The window each shifter was built with, in frames.
    pub fn window(&self) -> usize {
        self.window
    }

    /// How many choir instances have a shifter.
    pub fn instances(&self) -> usize {
        self.voices.len()
    }

    pub(crate) fn voice(&self, i: usize) -> Option<&ShiftVoice> {
        self.voices.get(i)
    }

    pub(crate) fn voice_mut(&mut self, i: usize) -> Option<&mut ShiftVoice> {
        self.voices.get_mut(i)
    }

    #[cfg(test)]
    pub(crate) fn unfaulted_for_test(
        channels: usize,
        instances: usize,
        sample_rate: f32,
        window: usize,
    ) -> Self {
        let mut v = Self::new(channels, instances, sample_rate, window);
        v.voices = (0..v.voices.len())
            .map(|i| ShiftVoice::unfaulted(channels, window, sample_rate, 7 + i as u64))
            .collect();
        v.snapshot.sv = ShiftVoice::unfaulted(channels, window, sample_rate, 7);
        v
    }

    /// Forgets the snapshot: the source, or what it holds, has changed.
    pub(crate) fn invalidate(&mut self) {
        self.snapshot.key = None;
    }

    /// Hits instance `i` at `pos` in direction `dir`. When `key` matches the
    /// snapshot the instance copies it (no budget cost). Otherwise a cold
    /// `output_seek` runs into the snapshot if `budget` allows, and the
    /// instance copies that; so the instances of a choir that share a key
    /// cost one seek between them. With the budget spent nothing changes and
    /// the result is `Refused`.
    pub(crate) fn resolve_hit(
        &mut self,
        i: usize,
        key: HitKey,
        src: &dyn ReadPath,
        (pos, dir): (f64, f64),
        p: ShiftParams,
        budget: &mut ColdBudget,
    ) -> HitOutcome {
        let cold = self.snapshot.key != Some(key);
        if i >= self.voices.len() {
            return HitOutcome::Refused;
        }
        if cold {
            if !budget.take(self.cold_cost) {
                #[cfg(test)]
                stats::bump(2);
                return HitOutcome::Refused;
            }
            self.snapshot.sv.hit(src, pos, dir, p);
            self.snapshot.key = Some(key);
            #[cfg(test)]
            stats::bump(0);
        }
        self.voices[i].hit_from(&self.snapshot);
        #[cfg(test)]
        if !cold {
            stats::bump(1);
        }
        match cold {
            true => HitOutcome::Cold,
            false => HitOutcome::Warm,
        }
    }
}

/// Counts of hit outcomes, for tests (thread-local: each test is its own
/// thread).
#[cfg(test)]
pub(crate) mod stats {
    use std::cell::Cell;
    thread_local! {
        static COUNTS: Cell<[u32; 3]> = const { Cell::new([0; 3]) };
    }
    pub(crate) fn reset() {
        COUNTS.with(|c| c.set([0; 3]));
    }
    pub(crate) fn bump(i: usize) {
        COUNTS.with(|c| {
            let mut v = c.get();
            v[i] += 1;
            c.set(v);
        });
    }
    /// (cold seeks, snapshot copies, fallbacks to the grains)
    pub(crate) fn get() -> (u32, u32, u32) {
        let v = COUNTS.with(Cell::get);
        (v[0], v[1], v[2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Saw;
    impl ReadPath for Saw {
        fn sample(&self, frame: usize, ch: usize) -> f32 {
            ((frame as f32 * 0.013 + ch as f32).fract() - 0.5) * 0.6
        }
        fn playing(&self, pos: f64) -> bool {
            (0.0..40000.0).contains(&pos)
        }
        fn step(&self, pos: &mut f64, dir: f64) -> bool {
            *pos += dir;
            false
        }
    }

    // the prefault leaves a set rendering exactly what an untouched one does
    #[test]
    fn prefault_changes_nothing_observable() {
        let p = ShiftParams {
            speed: 1.0,
            transpose: 1.5,
            rate_sign: 1.0,
        };
        for ch in [1usize, 2] {
            let mut a = VoiceShifters::new(ch, 2, 48000.0, 4320);
            let mut b = VoiceShifters::unfaulted_for_test(ch, 2, 48000.0, 4320);
            assert!(a.snapshot.key.is_none());
            for i in 0..2 {
                let (x, y) = (a.voice_mut(i).unwrap(), b.voice_mut(i).unwrap());
                x.hit(&Saw, 100.0, 1.0, p);
                y.hit(&Saw, 100.0, 1.0, p);
                let mut heard = 0.0f32;
                for n in 0..3000 {
                    let (fx, fy) = (x.frame().to_vec(), y.frame().to_vec());
                    for (u, v) in fx.iter().zip(&fy) {
                        assert_eq!(u.to_bits(), v.to_bits(), "ch {ch} inst {i} frame {n}");
                        heard = heard.max(u.abs());
                    }
                    x.advance(&Saw, p);
                    y.advance(&Saw, p);
                }
                assert!(heard > 0.05);
            }
        }
    }
}
