//! Buffers (RECb, REQ-095): memory that voices play while the engine records
//! into it.
//!
//! A buffer's samples are `AtomicU32`s (f32 bits) behind an `Arc`, allocated
//! on the command thread. The engine is the only writer; every voice playing
//! the buffer holds the `Arc` and reads. A sample is one atomic word, so a
//! read never tears: it sees the old or the new value. A per-half count of
//! the frames recorded is stored `Release` and loaded `Acquire`, so a reader
//! never sees a count ahead of its samples.
//!
//! A buffer has one half when it is updated live, and two otherwise (the
//! default): a take records into the back half while voices hear the front,
//! and the halves swap when the take ends.
//!
//! The command thread copies what voices hear with `copy_heard`. A take that
//! starts writing into the half being copied (the halves swapped and the next
//! take began during the copy) is seen through a per-half count of resets,
//! as in a seqlock, and the copy is made again, so it never holds part of one
//! take and part of the next.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering, fence};

use crate::audio_processing::commands::{GroupId, VoiceId};
use crate::export::Quantizer;

/// What a buffer records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufSource {
    /// The whole mix, before the output clamp.
    Out,
    /// One voice, after its effects, pan and gain (before its group).
    Voice(VoiceId),
    /// One group's bus, after its effects and gain.
    Group(GroupId),
}

/// The memory for a buffer could not be had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoMemory {
    /// How much was asked for.
    pub bytes: u128,
}

impl std::fmt::Display for NoMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not enough memory ({})", human_bytes(self.bytes))
    }
}

impl std::error::Error for NoMemory {}

/// `1.2 GB`, `640 kB`, `12 B`.
pub fn human_bytes(bytes: u128) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut x = bytes as f64;
    let mut unit = 0;
    while x >= 1000.0 && unit + 1 < UNITS.len() {
        x /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ => format!("{x:.1} {}", UNITS[unit]),
    }
}

/// A buffer's shared memory.
pub struct BufData {
    /// Interleaved channels per frame.
    pub channels: u16,
    frames: usize,
    // 1 half (live) or 2 (the default)
    halves: Box<[Box<[AtomicU32]>]>,
    // the half voices hear
    front: AtomicUsize,
    // frames recorded, per half (Release on write, Acquire on read)
    filled: Box<[AtomicUsize]>,
    // times each half was reset for a take to write into it from its start
    // (a seqlock's sequence: `copy_heard` sees a take begin under it)
    resets: Box<[AtomicUsize]>,
}

impl BufData {
    /// Sets aside `frames` frames of `channels` samples in `halves` halves
    /// (1 or 2), all silent. Runs on the command thread.
    pub fn alloc(frames: usize, channels: u16, halves: usize) -> Result<Arc<Self>, NoMemory> {
        let halves = halves.clamp(1, 2);
        let channels = channels.max(1);
        let bytes = (frames as u128) * u128::from(channels) * (halves as u128) * 4;
        let samples = frames
            .checked_mul(usize::from(channels))
            .filter(|_| bytes <= isize::MAX as u128)
            .ok_or(NoMemory { bytes })?;
        let mut made = Vec::new();
        for _ in 0..halves {
            let mut half: Vec<AtomicU32> = Vec::new();
            half.try_reserve_exact(samples)
                .map_err(|_| NoMemory { bytes })?;
            half.resize_with(samples, || AtomicU32::new(0));
            made.push(half.into_boxed_slice());
        }
        Ok(Arc::new(Self {
            channels,
            frames,
            halves: made.into_boxed_slice(),
            front: AtomicUsize::new(0),
            filled: (0..halves).map(|_| AtomicUsize::new(0)).collect(),
            resets: (0..halves).map(|_| AtomicUsize::new(0)).collect(),
        }))
    }

    /// Frames in a half.
    pub fn capacity(&self) -> usize {
        self.frames
    }

    /// 1 (live) or 2.
    pub fn halves(&self) -> usize {
        self.halves.len()
    }

    /// Frames voices can play: those recorded into the half they hear.
    pub fn frames_heard(&self) -> usize {
        let front = self.front.load(Ordering::Acquire);
        self.filled[front].load(Ordering::Acquire).min(self.frames)
    }

    /// The sample at `frame`, `ch`, in the half voices hear. Out of range
    /// reads silence.
    pub fn read(&self, frame: usize, ch: usize) -> f32 {
        let front = self.front.load(Ordering::Relaxed);
        self.halves[front]
            .get(frame * usize::from(self.channels) + ch)
            .map_or(0.0, |s| f32::from_bits(s.load(Ordering::Relaxed)))
    }

    /// The half voices hear now; a voice loads it once per frame and reads
    /// with `read_in`.
    pub fn heard_half(&self) -> usize {
        self.front.load(Ordering::Relaxed)
    }

    /// Like `read`, from the half `front` (from `heard_half`).
    pub fn read_in(&self, front: usize, frame: usize, ch: usize) -> f32 {
        self.halves[front]
            .get(frame * usize::from(self.channels) + ch)
            .map_or(0.0, |s| f32::from_bits(s.load(Ordering::Relaxed)))
    }

    /// A copy of what voices hear: the frames recorded into the half they
    /// hear, channels interleaved. Command thread only (it allocates). A take
    /// that begins writing into that half during the copy is seen, and the
    /// copy is made again from the half voices hear then. (A live buffer, of
    /// one half, is copied as it is being written.)
    pub fn copy_heard(&self) -> Vec<f32> {
        self.copy_heard_while(|_| ())
    }

    // `copy_heard`, calling `at_frame` before each frame is copied (tests:
    // the engine at work during a copy)
    pub(crate) fn copy_heard_while(&self, mut at_frame: impl FnMut(usize)) -> Vec<f32> {
        let channels = usize::from(self.channels);
        loop {
            // the resets before the half is chosen: a take that begins in it
            // from here on shows as a change
            let resets: [usize; 2] = std::array::from_fn(|h| {
                self.resets.get(h).map_or(0, |r| r.load(Ordering::Acquire))
            });
            let front = self.front.load(Ordering::Acquire);
            let heard = self.filled[front].load(Ordering::Acquire).min(self.frames);
            if self.front.load(Ordering::Acquire) != front {
                continue;
            }
            let mut out = Vec::with_capacity(heard * channels);
            for (f, frame) in self.halves[front][..heard * channels]
                .chunks(channels)
                .enumerate()
            {
                at_frame(f);
                out.extend(
                    frame
                        .iter()
                        .map(|s| f32::from_bits(s.load(Ordering::Relaxed))),
                );
            }
            // pairs with `reset_filled`'s fence: a sample written after a
            // reset, if copied, makes the reset visible here
            fence(Ordering::Acquire);
            if self.resets[front].load(Ordering::Relaxed) == resets[front] {
                return out;
            }
        }
    }

    // engine side ----------------------------------------------------

    fn front(&self) -> usize {
        self.front.load(Ordering::Relaxed)
    }

    fn write(&self, half: usize, frame: usize, ch: usize, value: f32) {
        if let Some(s) = self.halves[half].get(frame * usize::from(self.channels) + ch) {
            s.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    // after the samples of frames `0..n` are written
    fn publish(&self, half: usize, n: usize) {
        if n > self.filled[half].load(Ordering::Relaxed) {
            self.filled[half].store(n, Ordering::Release);
        }
    }

    // before a take writes into `half` from its start
    fn reset_filled(&self, half: usize) {
        self.resets[half].fetch_add(1, Ordering::Relaxed);
        // the reset is ordered before the take's samples (`copy_heard`)
        fence(Ordering::Release);
        self.filled[half].store(0, Ordering::Release);
    }

    fn flip_to(&self, half: usize) {
        self.front.store(half, Ordering::Release);
    }
}

/// The private copy of a take (REQ-095 point 5): a box the command thread
/// allocated, written alongside the buffer, handed back when the take ends.
pub struct SaveBox {
    /// Tells the mirror where the take goes.
    pub ticket: u64,
    /// `length x channels` samples, interleaved.
    pub data: Box<[f32]>,
    /// Keep the signal as it came, not as the buffer's `bits` quantised it:
    /// a 16 or 24-bit file is dithered once, by its writer.
    pub raw: bool,
}

/// A finished take's copy: ticket, samples, and the furthest frame written.
pub type SavedTake = (u64, Box<[f32]>, u32);

/// A buffer in the engine: where it records from and where the take is.
pub struct BufferSlot {
    pub(crate) data: Arc<BufData>,
    pub(crate) source: BufSource,
    pos: usize,
    // the half being written
    half: usize,
    pub(crate) recording: bool,
    paused: bool,
    looping: bool,
    // bits < 32: the quantiser and its scale
    quant: Option<(Quantizer, f32)>,
    // the take's private copy (`to=`), and the furthest frame written to it
    save: Option<SaveBox>,
    saved_to: usize,
    // the number the current take's `start` carried
    take_no: u32,
}

impl BufferSlot {
    /// An idle buffer. `bits` below 32 quantises what is recorded, with
    /// dither seeded by `seed`.
    pub fn new(
        data: Arc<BufData>,
        source: BufSource,
        looping: bool,
        bits: u8,
        seed: u64,
    ) -> BufferSlot {
        let quant = (bits < 32).then(|| {
            let bits = u32::from(bits.max(2));
            (Quantizer::new(bits, seed), (1u32 << (bits - 1)) as f32)
        });
        BufferSlot {
            data,
            source,
            pos: 0,
            half: 0,
            recording: false,
            paused: false,
            looping,
            quant,
            save: None,
            saved_to: 0,
            take_no: 0,
        }
    }

    /// The number of the current (or last) take.
    pub fn take_no(&self) -> u32 {
        self.take_no
    }

    /// The take's copy and its frames, if it has one; the slot lets go of it.
    pub fn take_save(&mut self) -> Option<SavedTake> {
        let saved = self.saved_to as u32;
        self.saved_to = 0;
        self.save.take().map(|s| (s.ticket, s.data, saved))
    }

    /// Takes the buffer apart, so what it owns can leave as garbage.
    pub fn into_parts(mut self) -> (Arc<BufData>, Option<SavedTake>) {
        let save = self.take_save();
        (self.data, save)
    }

    /// Whether the take is running (not paused).
    pub fn is_writing(&self) -> bool {
        self.recording && !self.paused
    }

    /// Frames of the current take written so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Records from the beginning. With two halves the take goes into the
    /// one voices don't hear; a live buffer keeps its old contents audible
    /// beyond the position.
    ///
    /// `save` is this take's private copy (`to=`). One left over from a take
    /// that was running is handed back.
    pub fn start(&mut self, save: Option<SaveBox>, take_no: u32) -> Option<SavedTake> {
        let old = self.take_save();
        self.save = save;
        self.take_no = take_no;
        self.pos = 0;
        self.recording = true;
        self.paused = false;
        if self.data.halves() == 2 {
            self.half = 1 - self.data.front();
            self.data.reset_filled(self.half);
        } else {
            self.half = 0;
        }
        old
    }

    pub fn pause(&mut self) {
        if self.recording {
            self.paused = true;
        }
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Ends the take; the frames recorded, if one was running.
    pub fn stop(&mut self) -> Option<u32> {
        self.recording.then(|| self.end())
    }

    // ends the take: the halves swap, unless nothing was recorded
    fn end(&mut self) -> u32 {
        if self.data.halves() == 2 && self.pos > 0 {
            self.data.flip_to(self.half);
        }
        self.recording = false;
        self.paused = false;
        self.pos as u32
    }

    /// Writes one frame to the take. Returns the frames recorded if the
    /// frame filled the buffer, which ended the take (a looping buffer wraps
    /// instead).
    pub fn push(&mut self, frame: &[f32]) -> Option<u32> {
        if !self.is_writing() {
            return None;
        }
        let capacity = self.data.capacity();
        if capacity == 0 {
            return Some(self.end());
        }
        for ch in 0..usize::from(self.data.channels) {
            let raw = frame.get(ch).copied().unwrap_or(0.0);
            let mut x = raw;
            if let Some((q, scale)) = &mut self.quant {
                x = q.quantize(x) as f32 / *scale;
            }
            self.data.write(self.half, self.pos, ch, x);
            if let Some(save) = &mut self.save
                && let Some(cell) = save
                    .data
                    .get_mut(self.pos * usize::from(self.data.channels) + ch)
            {
                *cell = if save.raw { raw } else { x };
            }
        }
        self.pos += 1;
        self.saved_to = self.saved_to.max(self.pos);
        self.data.publish(self.half, self.pos);
        if self.pos < capacity {
            return None;
        }
        if self.looping {
            // a pass over a two-half buffer is a whole take: it swaps in,
            // and the next pass records into the half just given up
            if self.data.halves() == 2 {
                self.data.flip_to(self.half);
                self.half = 1 - self.half;
                self.data.reset_filled(self.half);
            }
            self.pos = 0;
            return None;
        }
        Some(self.end())
    }
}
