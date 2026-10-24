# RECa: WAV export writer (`export/**`)
Status: draft   ·   Changelog at the bottom

Part 1 of 3 of REC (brief `briefs/PJ-recording.md`, wave 4). RECa, RECb and
RECc run in sequence in wave 4. RECa owns only `export/**` and needs nothing
from the engine, so it could start as soon as wave 4 is scaffolded.

## Summary
A new top-level module `export/` that turns a stream of interleaved f32 blocks
(read from an `rtrb` ring) into WAV files on a dedicated writer thread: 32-bit
float by default, or 16/24-bit integer with clamping and TPDF dither done on
the writer thread. It handles end-of-stream (producer dropped), periodic header
checkpoints, the ~4 GB WAV limit, default file names, completion notices, and a
shutdown join. Nothing in the engine or the command layer uses it yet (RECb
wires it).

## Ownership
- **Creates:** `blast/src/export/mod.rs` (public API, `RecHandle`, notifier,
  `finish_all`), `export/writer.rs` (writer thread loop, file splitting),
  `export/dither.rs` (`Quantizer`), `export/naming.rs` (default path, UTC
  stamp, part names).
- **Modifies:** top-level `README.md` module list (add `src/export`), docs only.
- **Reads only:** `audio_processing/blast_rand.rs` (`X128P`), hound 3.5.1,
  rtrb 0.4.0 sources.

## Registry entries
- `blast/src/lib.rs`: `pub mod export;`
- Stub `blast/src/export/mod.rs` containing only `//! WAV recording and export (REC).`
  (the architect creates it so `lib.rs` compiles; RECa replaces it).

## Architecture changes
The recording ring and the writer thread are a new queue and a new thread
(README §2 architecture rule). The brief prescribes them; filed for the record
as **REQ-001** (proposed README §3.10, recording pipeline). No new crates
(`hound`, `rtrb` are in README §1.1).

## Interfaces
**Provides** (`crate::export`):
```rust
/// Sample format of the written file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind { Float32, Int16, Int24 }

#[derive(Clone, Copy, Debug)]
pub struct RecSpec { pub sample_rate: u32, pub channels: u16, pub kind: SampleKind }

#[derive(Debug)]
pub enum RecError {
    BadSpec(&'static str),                       // 0 channels, 0 rate
    Create { path: PathBuf, source: hound::Error },
    Spawn(std::io::Error),
}
// + hand-written Display and std::error::Error (README §1.3)

/// Opens the file, allocates the ring, spawns the writer. REPL thread only.
pub fn start(path: &Path, spec: RecSpec) -> Result<(rtrb::Producer<f32>, RecHandle), RecError>;
/// Same, with tunables; used by tests (small ring, small size cap, fast poll).
pub fn start_with(path: &Path, spec: RecSpec, opts: WriterOpts)
    -> Result<(rtrb::Producer<f32>, RecHandle), RecError>;

#[derive(Clone, Copy, Debug)]
pub struct WriterOpts {
    pub ring_seconds: u32,        // default RING_SECONDS = 4
    pub max_data_bytes: u32,      // default MAX_DATA_BYTES (see step 4)
    pub poll: Duration,           // default 10 ms
    pub checkpoint: Duration,     // default 1 s
}

/// Cheap to clone (an `Arc`), so the command mirror can hold it.
#[derive(Clone)]
pub struct RecHandle(Arc<Shared>);
impl RecHandle {
    pub fn path(&self) -> &Path;                 // first file of the take
    pub fn status(&self) -> RecStatus;
    pub fn is_running(&self) -> bool;
    pub fn request_stop(&self);                  // fallback end-of-stream (shutdown)
    pub fn wait(&self, timeout: Duration) -> bool;   // true if finished
}
#[derive(Clone, Debug, PartialEq)]
pub enum RecStatus {
    Running  { frames: u64, files: u32 },
    Finished { frames: u64, files: u32, bytes: u64 },
    Failed(String),
}

/// Registers the line printer used for completion and error notices.
pub fn set_notifier(f: fn(&str));
/// Asks every live writer to stop, then waits up to `timeout` in total.
pub fn finish_all(timeout: Duration);

// export::naming
pub fn default_path(dir: &Path, now: SystemTime) -> PathBuf;   // dir/2026-10-24_15-30-12.wav, -2, -3… if taken
pub fn utc_stamp(unix_secs: u64) -> String;                      // "YYYY-MM-DD_HH-MM-SS"
pub fn part_path(first: &Path, part: u32) -> PathBuf;            // part 1 → first; 2 → stem-002.ext
```
**Consumes:** `rtrb::RingBuffer::<f32>::new`, `Consumer::{pop_partial_slice,
is_abandoned}`; `hound::WavWriter::{create, write_sample, flush, finalize}`;
`blast_rand::X128P::{new, next_f32}`.

**Contract issues:** none for RECa itself. The notifier needs R1 to expose a
line printer that is safe to call from a non-audio, non-REPL thread (REQ-001).
If none exists, `set_notifier` is simply never called and results show in
`rec` status (RECb).

### hound 3.5.1 facts checked (in `~/.cargo/registry/src/*/hound-3.5.1/`)
- 32-bit float: `write.rs:353-359` writes format tag 3 (≤2 channels);
  `write.rs:408-414` writes the IEEE-float subformat for WAVE_FORMAT_EXTENSIBLE
  (>2 channels). `f32::write_padded` accepts only (32, 4) (`lib.rs:296-300`).
- 24-bit: `i32::write_padded` (24, 3) (`lib.rs:262`); bits > 16 selects
  WAVE_FORMAT_EXTENSIBLE (`write.rs:234-238`). 16-bit: `i16`, PCMWAVEFORMAT.
- Header patch: `finalize` (`write.rs:540-552`) calls `update_header`
  (`write.rs:488-512`), which seeks back and writes the RIFF and data sizes;
  `flush` (`write.rs:527-533`) does the same as a checkpoint and keeps writing
  possible; `Drop` (`write.rs:579-589`) patches silently if not finalised.
- **Limit:** `data_bytes_written` is a `u32` (`write.rs:436`, `+=`), so it
  overflows past 4 GiB (debug panic, release corrupt header). The writer must
  enforce its own cap (step 4, REQ-003). No `fact` chunk is written for float;
  common readers (and symphonia) don't need it.
- Odd-sized data chunks (24-bit mono with an odd frame count) aren't padded
  to even length. Harmless for every reader we use; noted only.

## Steps
1. **`export/naming.rs`.** `utc_stamp` (civil-from-days on `unix_secs / 86400`,
   no crate; UTC, because local time needs tz data we don't have), `default_path`
   (`recordings/<stamp>.wav` relative to the given dir; if the file exists append
   `-2`, `-3`, …), `part_path`. Pure functions; unit tests.
2. **`export/dither.rs`.** `pub struct Quantizer { scale: f32, min: i32, max: i32, rng: X128P }`,
   `Quantizer::new(bits: u32, seed: u64)`, `fn quantize(&mut self, x: f32) -> i32`:
   `x.clamp(-1.0, 1.0) * scale + (rng.next_f32() - rng.next_f32())`, rounded,
   then clamped to `[min, max]` with `scale = 2^(bits-1)`, `min = -scale`,
   `max = scale - 1`. The dither is TPDF, ±1 LSB peak. NaN maps to 0
   (`clamp` keeps NaN, so test `is_nan` first). Seed from
   `SystemTime::now()` nanos in `start` (not `blast_rand::fast_seed`, which
   uses `unsafe` that R1 is removing).
3. **`export/writer.rs`: the loop.** `fn run(consumer, shared, spec, opts, first_path, writer)`
   on the spawned thread (named `blast-rec`). It allocates freely (it isn't the
   audio thread): a scratch `Vec<f32>` of `channels * 4096` samples.
   ```text
   loop {
       n = consumer.pop_partial_slice(&mut scratch).0.len()
       if n > 0 { write(&scratch[..n])?; checkpoint if opts.checkpoint elapsed; continue }
       if consumer.is_abandoned() || shared.stop.load(Acquire) {
           drain: pop until empty, writing each chunk   // see "flush" below
           break
       }
       sleep(opts.poll)
   }
   finalize current file; if the take wrote 0 frames, delete the file
   set status Finished/Failed, notify, signal `done` (Mutex<bool> + Condvar)
   ```
   **Flush guarantee.** The engine pushes whole blocks with
   `push_entire_slice`, which commits with a Release store before the call
   returns. The producer is dropped later on the REPL thread (after the
   garbage round trip, RECb), which sets the abandoned flag. The writer reads
   the flag with Acquire (`rtrb lib.rs:741-743`), so once it sees "abandoned",
   every committed sample is visible, and the final drain gets all of them.
   `write` converts per `SampleKind`: `Float32` writes the f32 unchanged
   (overs survive), `Int16`/`Int24` go through `Quantizer`. Any hound error:
   status `Failed(msg)`, notify, finalise what's possible, then drop the
   consumer. The engine sees `is_abandoned` and retires its producer (RECb).
4. **File size cap and splitting** (REQ-003, recommended option). Per file,
   `max_samples = (opts.max_data_bytes / block_align) * channels` where
   `block_align = channels * bytes_per_sample`, and default
   `MAX_DATA_BYTES = u32::MAX - 4096` (leaves room for the 68-byte header in
   the RIFF size). `write` counts samples in the current file. When the next
   chunk would cross the cap, it writes up to the cap (a frame boundary,
   because the cap is a multiple of `channels`), finalises, opens
   `part_path(first, k+1)`, notifies "rec: continuing in <path>", and writes the
   rest. If REQ-003 picks "stop", the writer instead finalises, sets
   `Failed("size limit reached")` and drops the consumer; only this step and
   its test change.
5. **Checkpoints.** Every `opts.checkpoint` (1 s) of wall time, call
   `WavWriter::flush()`, so a crash or Ctrl-C leaves a valid file up to the
   last second (hound documents this use).
6. **`export/mod.rs`: API and bookkeeping.** `start`/`start_with`:
   validate the spec, `WavWriter::create` on the calling (REPL) thread so path
   and permission errors return synchronously as `RecError::Create`, allocate
   the ring (`ring_seconds * sample_rate * channels` f32: 4 s is 1.5 MB for
   48 kHz stereo and 24 MB for 192 kHz × 8 ch; it absorbs filesystem stalls of
   up to 4 s while the writer polls every 10 ms), spawn the thread with
   `std::thread::Builder` (spawn errors map to `RecError::Spawn`), and add the
   handle to a private `static LIVE: Mutex<Vec<Weak<Shared>>>` (pruned on each
   `start`). `Shared` holds `path`, `stop: AtomicBool`,
   `status: Mutex<RecStatus>`, `done: (Mutex<bool>, Condvar)`.
   `set_notifier` stores a `fn(&str)` in a `OnceLock`; `notify` falls back to
   doing nothing. `finish_all` sets `stop` on every live handle, then waits on
   each against a shared deadline.
7. Doc comments on every `pub` item; update the top-level `README.md` module
   list with `src/export`; `cargo clippy` clean in `export/**`.

## Tests
All in `export/`, writing temp files under
`std::env::temp_dir().join(format!("blast-rec-{}-{name}", std::process::id()))`,
read back with `hound::WavReader`, and removed at the end. No fixtures.
- `naming::utc_stamp_epoch_and_leap_day`: `0` → `1970-01-01_00-00-00`,
  `951_782_400` → `2000-02-29_00-00-00`, `951_868_799` → `2000-02-29_23-59-59`.
- `naming::default_path_avoids_existing_file`: creates the stamped file, gets `…-2.wav`.
- `naming::part_path_numbers_from_two`: `take.wav` → `take.wav`, `take-002.wav`, `take-010.wav`.
- `dither::quantize_stays_within_one_lsb`: 10 000 random inputs; `|q - x*scale| <= 1.5`.
- `dither::dither_is_triangular`: fixed seed, constant input 0.0 at 16 bits:
  outputs only in {-1, 0, 1}, mean within 0.02 of 0, P(0) ≈ 0.5 ± 0.05.
- `dither::clamps_overs_and_nan`: 2.0 → max, -2.0 → min, NaN → 0 (±1 LSB).
- `writer::float_roundtrip_keeps_overs`: push 3 blocks including ±1.7, drop the
  producer, `wait`; the file is IEEE float, 2 ch, the right rate, and bit-exact.
- `writer::int24_header_and_values`: 24-bit, 3 channels (extensible header);
  every sample within 1 LSB of `x * 2^23`.
- `writer::int16_roundtrip`: same for 16-bit stereo.
- `writer::flushes_everything_after_producer_drop`: 50 000 frames pushed in
  odd-sized blocks while the writer runs (5 ms poll); all of them land in the file.
- `writer::split_at_size_limit`: `max_data_bytes = 1000`, float stereo
  (block_align 8 → 125 frames per file); 300 frames → files of 125, 125, 50 frames,
  named `take.wav`, `take-002.wav`, `take-003.wav`; the concatenation equals the input.
- `writer::stop_flag_ends_take_while_producer_alive`: keep the producer,
  call `request_stop`; `wait` returns true and the file holds what was pushed.
- `writer::empty_take_deletes_file`: drop the producer without pushing; the
  file is gone and the status is `Finished { frames: 0, .. }`.
- `mod::create_error_is_synchronous`: path inside a nonexistent directory gives
  `RecError::Create`; no thread is left running.
- `mod::bad_spec_rejected`: 0 channels and 0 rate give `RecError::BadSpec`.

## Done when
- README §1.4 holds, and every test above passes.
- No `unwrap`/`expect` on I/O or user paths. `Mutex` lock poisoning is
  handled by `unwrap_or_else(PoisonError::into_inner)`.
- Nothing in `export/**` is reachable from `render`/`apply` except the
  `Producer` type (checked again in RECb's review).

## Risks and open questions
- Size-limit policy: **REQ-003** (step 4 and its test depend on it).
- Pipeline approval (thread + ring as a §3 contract): **REQ-001**. RECa's code
  doesn't change with the outcome unless the architect redesigns it.
- `recordings/` will appear in the working directory; ask the architect to add
  it to `.gitignore` in the wave-4 scaffold (not a REC-owned file).
- Nice-to-have, not planned: a `fact` chunk for float files, BWF `bext`
  timestamps, RF64 (would need a hand-written writer).

## Size
M: four small files, ~350 lines plus ~250 lines of tests; no engine coupling.

## Changelog
- 2026-10-24: first draft (planner PJ).
