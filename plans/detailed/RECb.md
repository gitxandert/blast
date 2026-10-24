# RECb: live recording (`rec start` / `rec stop`, render tap)
Status: draft   ·   Changelog at the bottom

Part 2 of 3 of REC (brief `briefs/PJ-recording.md`, wave 4). Runs after RECa
in wave 4.

## Summary
`rec start [path] [--bits 16|24]`, `rec stop` and `rec` (status) work. The
engine copies every rendered block, **before** the backend's clamp, into the
take's ring without blocking or allocating, drops whole blocks on overflow and
reports them with an event, and hands the ring's producer back through the
garbage queue on stop. The default output is 32-bit float WAV at the output
rate and channel count (README §4 item 6).

## Ownership
- **Modifies:** `engine/render.rs` (all of it: `Conductor::render` is split
  into the existing mix, renamed `mix_block`, plus the tap).
- **Creates:** `engine/handlers/record.rs`, `commands/record.rs`.
- **Reads only:** `export/**` (RECa), `engine/mod.rs` (Conductor, `apply`),
  `engine/events.rs`, the garbage type (E1), `commands/mod.rs`,
  `commands/reprs.rs`, `commands/errors.rs`, `blast_time.rs` (`sample_rate::get`),
  `runtime/**` (R1: where `render` and the clamp are called).

Names below that E1/R1 haven't fixed yet (`Garbage`, `discard`, `emit`,
`EngineState.out_channels`, `CmdErr`) are this plan's assumptions; the
wave-4 refresh (planner Job 2) aligns them, editorially, with what's on
`integrate/wave-4`.

## Registry entries
The architect adds these in the wave-4 scaffold (stubs compile and do nothing):
1. `commands/mod.rs`: `pub mod record;` and the re-exports
   `pub use record::{RecStartArgs, RecStopArgs};`.
2. `commands!` list: `RecStart, RecStop` (args structs live in
   `commands/record.rs`: `pub struct RecStartArgs { pub producer: rtrb::Producer<f32> }`,
   `pub struct RecStopArgs {}`).
3. Parse dispatch in `commands/mod.rs`: `"rec" => self.try_rec(rest)`, with
   `try_rec(&mut self, args: &str) -> CmdResult<Parsed>` stubbed in
   `commands/record.rs` as `Err(CmdErr::Record("not implemented".into()))`.
   If C1's dispatch arms return `CmdResult<Command>`, the arm is
   `"rec" => return self.try_rec(rest)` placed before the `Parsed::Engine` wrap.
4. `commands/errors.rs`: `CmdErr::Record(String)` with Display
   `"rec: {msg}"`.
5. The HELP table aggregation (INS): include `record::HELP`.
6. `commands/reprs.rs`: `EngineState` field `pub rec: record::RecRepr`,
   initialised with `RecRepr::default()`. `RecRepr: Clone + Default`.
7. `engine/mod.rs`: `Conductor` field `rec: render::RecTap`, initialised with
   `RecTap::new()` (no allocation); `apply` arms
   `Command::RecStart(a) => self.rec_start(a)`, `Command::RecStop(a) => self.rec_stop(a)`.
8. `engine/handlers/mod.rs`: `mod record;` (stub file with empty
   `rec_start`/`rec_stop` taking the args).
9. `engine/events.rs`: `Event::RecDropped { samples: u32 }`, `Event::RecAborted`,
   with `describe` arms `"rec: ring overflow, dropped {samples} samples"` and
   `"rec: writer stopped (see `rec`)"`.
10. Garbage enum (E1): `Recorder(rtrb::Producer<f32>)`.
11. Runtime (R1's files, by registry, since nobody owns them in wave 4): at
    startup `crate::export::set_notifier(<R1's thread-safe line printer>)`;
    in the quit path, after the audio stream is dropped and before exit,
    `crate::export::finish_all(Duration::from_secs(5))`.
12. `.gitignore`: `recordings/` (convenience, see RECa risks).

## Architecture changes
**REQ-001**: the recording pipeline as README §3.10 (ring + writer thread,
the Command/Garbage/Event variants, notifier and shutdown hooks). No crates.

## Interfaces
**Provides**
```rust
// engine/render.rs
pub struct RecTap {
    live: Option<Producer<f32>>,
    retiring: [Option<Producer<f32>>; 4],   // waiting for room in the garbage queue
    dropped: u64,                           // samples dropped in the current overflow episode
}
impl RecTap { pub fn new() -> Self; }
impl Conductor {
    /// Unchanged signature (README §3.4): interleaved, unclamped.
    pub fn render(&mut self, out: &mut [f32], channels: usize);
}
// engine/handlers/record.rs
impl Conductor {
    pub(crate) fn rec_start(&mut self, args: RecStartArgs);
    pub(crate) fn rec_stop(&mut self, args: RecStopArgs);
}
// commands/record.rs
pub const HELP: &[(&str, &str)];
#[derive(Clone, Default)]
pub struct RecRepr { take: Option<RecHandle>, last: Option<RecHandle> }
impl RecRepr { pub fn is_recording(&self) -> bool; }   // take is Some and running
impl CmdProcessor { pub(crate) fn try_rec(&mut self, args: &str) -> CmdResult<Parsed>; }
```
**Consumes:** RECa's `export::{start, RecSpec, SampleKind, RecHandle, RecStatus,
default_path}`; E1's `Conductor::render` body, an event push that drops when
full (assumed `fn emit(&mut self, e: Event)`), and a garbage push that hands
the value back when full (assumed `fn discard(&mut self, g: Garbage) -> Result<(), Garbage>`);
`blast_time::sample_rate::get()`; the mirror's output channel count.

**Contract issues**
- REQ-001 asks E1 to make the garbage push return the value on a full queue.
  If E1 drops it instead, the tap can't use it (dropping a `Producer` whose
  writer already exited frees the ring on the audio thread). Fallback: the
  tap's `retiring` slots plus a dedicated check, still within RECb.
- If the mirror implements `Command::Batch` rollback by cloning
  `EngineState`, `RecRepr` is `Clone` (it holds `RecHandle`, an `Arc`). But
  `rec start` has a side effect (it opens a file and spawns the writer), which
  a rollback can't undo. It's still safe: the dropped command drops the
  producer, the writer finishes an empty take and deletes its file, and
  `is_recording()` turns false on its own (it checks the handle, not only
  `take.is_some()`). Noted for the REPL planner; no change needed.

## Steps
1. **`commands/record.rs`: syntax and mirror.** `try_rec` splits on
   whitespace:
   - `""` → `Parsed::Local(status)`: "recording to <path> (12.3 s)",
     "idle; last take: wrote <path> (3:02, 69 MB)" or "…failed: <msg>".
   - `"start" [path] [--bits 16|24]` (flag and path in either order): reject
     if `is_recording()` (`CmdErr::Record("already recording to <path>")`), parse
     `--bits` (anything else → error naming the allowed values), path defaults
     to `export::default_path(Path::new("recordings"), SystemTime::now())`
     after `create_dir_all("recordings")`. Spec: `sample_rate::get()`, the
     mirror's channel count (`u16::try_from`, error if it doesn't fit),
     `Float32` / `Int16` / `Int24`. Call `export::start`; map `RecError` to
     `CmdErr::Record(e.to_string())`. **Only after it succeeds**, set
     `rec.take = Some(handle)` and return
     `Parsed::Engine(Command::RecStart(RecStartArgs { producer }))`. A parse
     error leaves the mirror untouched.
   - `"stop"` → reject if not recording; move `take` into `last`; return
     `Command::RecStop(RecStopArgs {})`.
   - `HELP` rows: `("rec", "rec [start [path] [--bits 16|24] | stop]: record the output to WAV (32-bit float by default)")`.
2. **`engine/render.rs`: split and tap.** Rename E1's render body to
   `fn mix_block(&mut self, out: &mut [f32], channels: usize)` (no change in
   behaviour). New `render` = `self.mix_block(out, channels); self.tap(out);`.
   `fn tap(&mut self, block: &[f32])`:
   ```text
   self.retry_retiring()                       // one garbage push attempt per slot
   let Some(p) = self.rec.live.as_mut() else { return };
   if p.is_abandoned() { retire(live.take()); emit(RecAborted); return }   // writer died
   match p.push_entire_slice(block) {
       Ok(())  => if dropped > 0 { emit(RecDropped{ samples: min(dropped, u32::MAX) }); dropped = 0 }
       Err(_)  => dropped += block.len()        // whole block, keeps frame alignment
   }
   ```
   `push_entire_slice` is a wait-free copy into preallocated slots (rtrb
   `chunks.rs:307-317`); no allocation, no lock, no syscall. A whole-block drop
   leaves a gap in the file (time jumps), reported once per overflow episode
   rather than once per period, so a slow disk can't flood the event queue.
   The tap runs **after** mixing and **before** returning to the backend,
   which clamps (README §3.4/§3.6), so overs survive in float takes.
3. **`engine/handlers/record.rs`.** `rec_start`: if `live` is somehow `Some`
   (the mirror prevents it), retire the old one first, then
   `live = Some(args.producer)`, `dropped = 0`. `rec_stop`: if `dropped > 0`,
   emit `RecDropped`; `retire(live.take())`. `fn retire(&mut self, p)`:
   `discard(Garbage::Recorder(p))`; on `Err`, park it in the first free
   `retiring` slot; if all 4 are taken (garbage queue full for that many stops),
   drop it in place as a last resort and say so in a comment (it frees only
   if the writer has already exited).
4. **How the last samples get flushed** (the brief asks for this exactly):
   1. `rec stop` is parsed on the REPL thread and queued.
   2. At the start of the next callback the engine applies `RecStop`. Every
      block rendered before that was already pushed in full (or counted as
      dropped). The take ends on a period boundary.
   3. The producer goes into the garbage queue (or a `retiring` slot, retried
      every period).
   4. The REPL side drains garbage and drops the producer, which sets rtrb's
      abandoned flag (Release).
   5. The writer sees `is_abandoned()` (Acquire), drains the rest of the ring,
      finalises the header, deletes the file if it's empty, and notifies.
   At quit, `export::finish_all` (registry entry 11) covers a Conductor that
   is never dropped, through the writer's `stop` flag.
5. Doc comments, clippy clean in the three owned files.

## Tests
Engine tests go through E1's `test_util` harness (mirror + engine), with a
small ring from `export::start_with` (or a bare `rtrb::RingBuffer::new` where
only the engine is under test). Temp files as in RECa.
- `render::tap_copies_unclamped_block`: two voices summing to ±1.6;
  the consumer holds exactly the samples `render` wrote to `out`, overs intact.
- `render::tap_is_inert_when_idle`: `render` output is identical with and
  without a `RecTap` that never started (same as E1's existing render tests).
- `render::overflow_drops_whole_blocks_and_reports_once`: ring of 1.5 blocks;
  render 4 blocks without popping (block 1 fits, blocks 2–4 are dropped), then
  pop all and render one more: exactly one `RecDropped { samples: 3 * block_len }`
  event, emitted on the fifth render, and the consumer holds blocks 1 and 5
  only.
- `handlers::rec_stop_returns_producer_via_garbage`: after `RecStop` the
  garbage queue holds `Garbage::Recorder`, and `rec.live` is `None`.
- `handlers::full_garbage_queue_parks_and_retries`: fill the garbage queue,
  stop; the producer sits in `retiring`; drain garbage, render once; it's gone.
- `handlers::abandoned_writer_aborts_take`: drop the consumer; next `render`
  retires the producer and emits `RecAborted`.
- `record::rec_start_stop_end_to_end`: `run("rec start <tmp> --bits 24")`,
  render 10 blocks of a known voice, `run("rec stop")`, drain garbage (drop),
  `handle.wait(1 s)`; the file has 10 blocks of 24-bit samples within 1 LSB.
- `record::default_take_is_float_at_output_spec`: file spec is float, 32 bits,
  the configured rate and channel count (path given explicitly in the test, to
  keep the cwd clean; `default_path` is covered by RECa).
- `record::second_start_rejected_and_mirror_unchanged`.
- `record::stop_when_idle_rejected`.
- `record::bad_bits_rejected`: `--bits 8`, `--bits`, `--bits x`; mirror unchanged.
- `record::start_error_leaves_mirror_unchanged`: path in a nonexistent
  directory; `is_recording()` is false and nothing is queued.
- `record::status_reports_last_take`: after a finished take, `rec` returns a
  `Parsed::Local` line naming the file.

## Done when
- README §1.4 holds and every test above passes.
- Real-time trace (planner review): `render` → `mix_block` + `tap` →
  `push_entire_slice`, `is_abandoned`, `emit`, `discard`: no allocation,
  lock, print or I/O. `apply` → `rec_start`/`rec_stop`: moves and the same
  calls only.
- Manual check on the dev machine, noted in the report: record 10 s while
  playing a clipping mix; the float file shows peaks above 0 dBFS; the 16-bit
  file is clean and dithered; `rec` reports both.

## Risks and open questions
- **REQ-001** (pipeline as §3.10): registry entries 7–11 depend on it.
- Registry names (Garbage, `emit`, `discard`, `out_channels`) are
  assumptions until E1's plan is final; the refresh fixes them editorially.
- `rec start` inside a shell `&` batch works but is pointless, since it can't
  be rolled back; nothing is planned to forbid it.
- Overflow leaves a time gap rather than silence. Writing zeros for the
  gap would need a side channel; not planned.
- Nice-to-have, not planned: `rec pause`, recording a single voice/group
  (pre-fader stems), `--split <minutes>`.

## Size
M: three files, ~250 lines plus ~250 lines of tests; the risky part (the tap)
is ~30 lines.

## Changelog
- 2026-10-24: first draft (planner PJ).
