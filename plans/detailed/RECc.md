# RECc: offline bounce (`bounce <seconds> <path>`)
Status: draft (depends on REQ-002)   ·   Changelog at the bottom

Part 3 of 3 of REC (brief `briefs/PJ-recording.md`, wave 4). Runs after RECb
in wave 4. **Implemented only if REQ-002 picks option (a).** If the user picks
(c), this plan is withdrawn; if (b), it's replaced by an architect design.

## Summary
`bounce <seconds> [path] [--bits 16|24]` renders the live session's next N
seconds into a WAV file faster than real time (up to 8×), inside the audio
callback, reusing RECa's writer and RECb's tap. Live output is silent during
the bounce; an event reports completion.

## Options evaluated (brief's last bullet)
- **(a) Render inside the callback across several periods.** Recommended.
  Details below. No new thread or queue beyond REQ-001.
- **(b) Second Conductor from the mirror.** Rejected for this roadmap: the
  clock and sample rate are process-wide statics (`blast_time.rs:6-37`,
  `SAMPLE_COUNTER`, `SAMPLE_RATE`), so a second Conductor would advance the
  live engine's clock; and the mirror holds structure only, not play
  positions, tempo phases or RNG state, so the result wouldn't match the
  session. It needs those globals moved into the Conductor (touches E1, TMP,
  PRC and every process) plus a mirror-to-engine rebuild: an architecture
  change, L–XL.
- **(c) Out of scope.** Zero cost; `rec` covers real-time capture.
Filed as **REQ-002**.

## Ownership
Same files as RECb: `engine/render.rs`, `engine/handlers/record.rs`,
`commands/record.rs`. Reads `export/**`.

## Registry entries
(Added in the wave-4 scaffold only if REQ-002 = (a).)
1. `commands!` list: `Bounce` with `pub struct BounceArgs { pub producer: rtrb::Producer<f32>, pub frames: u64 }`
   in `commands/record.rs`, re-exported from `commands/mod.rs`.
2. Parse dispatch: `"bounce" => self.try_bounce(rest)`, stub returning
   `Err(CmdErr::Record("not implemented".into()))`.
3. `engine/mod.rs` `apply`: `Command::Bounce(a) => self.bounce(a)`.
4. `engine/events.rs`: `Event::BounceDone { frames: u64 }`, `describe`:
   `"bounce: done ({seconds:.1} s rendered)"` (seconds from
   `sample_rate::get()` on the REPL side).

## Architecture changes
REQ-002 (choice of design). The design itself adds no thread or queue
beyond REQ-001.

## Interfaces
**Provides**
```rust
// engine/render.rs, added to RecTap
bounce_left: u64,            // frames still to render; 0 = no bounce
pub const BOUNCE_BLOCKS: usize = 8;   // max extra blocks per callback
// engine/handlers/record.rs
impl Conductor { pub(crate) fn bounce(&mut self, args: BounceArgs); }
// commands/record.rs
impl CmdProcessor { pub(crate) fn try_bounce(&mut self, args: &str) -> CmdResult<Parsed>; }
```
`RecRepr` gains `mode: TakeMode { Rec, Bounce }` so `rec` status and the
start/stop rules know which is running.
**Consumes:** RECa `export::start`; RECb `RecTap`, `retire`, `tap` internals.

## Steps
1. **`try_bounce`.** `bounce <seconds> [path] [--bits 16|24]`: seconds is a
   positive finite f64, at most 24 h (rejects typos like `bounce 1e9`); frames
   = `round(seconds * rate)`. Reject if a take or bounce is running. Path
   default: `default_path("recordings", now)`. Same `export::start` flow and
   mirror rule as `rec start` (mirror only changes after success). `rec stop`
   also ends a running bounce early (the file keeps what was rendered).
   HELP row: `("bounce", "bounce <seconds> [path] [--bits 16|24]: render the next N seconds to WAV faster than real time; live output is muted meanwhile")`.
2. **`bounce` handler.** Same as `rec_start`, plus `bounce_left = args.frames`.
3. **`render` in bounce mode.** When `bounce_left > 0`:
   ```text
   for _ in 0..BOUNCE_BLOCKS {
       let frames = min(out.len() / channels, bounce_left)
       let block = &mut out[..frames * channels]
       if producer.slots() < block.len() { break }     // wait for the writer: never drop
       mix_block(block, channels); push_entire_slice(block)   // cannot fail now
       bounce_left -= frames
       if bounce_left == 0 { retire(live.take()); emit(BounceDone{frames: total}); break }
   }
   out.fill(0.0)                                      // live output muted
   ```
   No new buffer: the device buffer is the scratch block. Nothing is ever
   dropped (the writer's speed sets the pace), so the bounce is gapless.
   Rendering 8 blocks may overrun the callback deadline on heavy sessions;
   the output is silent, so the xrun is inaudible. `is_abandoned` handling
   is RECb's (aborts the bounce with `RecAborted`).
4. Document the semantics in HELP and in the `bounce` doc comment: renders
   the live session forward; live time advances by N seconds; commands
   typed during a bounce apply at the next block boundary in bounce time.

## Tests
- `render::bounce_renders_exact_frame_count`: bounce 1000 frames with a
  64-frame `out`; after enough renders the consumer holds exactly 1000 frames,
  identical to 1000 frames rendered live by a twin Conductor (same commands;
  a plain voice only, no tempo-driven processes, because the clock is global).
- `render::bounce_mutes_live_output`: every `out` during the bounce is all zeros.
- `render::bounce_is_faster_than_real_time`: with an empty ring, one `render`
  call advances the session by `BOUNCE_BLOCKS` blocks.
- `render::bounce_waits_for_ring_space`: ring of 2 blocks, no popping: renders
  2 blocks, then renders none (and drops none) until the consumer pops.
- `handlers::bounce_done_event_and_producer_retired`.
- `record::bounce_end_to_end`: `bounce 0.5 <tmp>` at 48 kHz; the file has
  24 000 frames.
- `record::bounce_rejected_while_recording` and `record::bad_seconds_rejected`
  (`0`, `-1`, `nan`, `1e9`, missing).
- `record::rec_stop_cancels_bounce`.

## Done when
- README §1.4 holds and every test above passes.
- Real-time trace as in RECb; the bounce loop calls only `mix_block`,
  `slots`, `push_entire_slice`, `fill`.
- Manual check: `bounce 30` of a running loop finishes in well under 30 s and
  matches by ear.

## Risks and open questions
- **REQ-002**: the whole plan depends on it.
- Bounce shares the global clock with live playback by design; scripts using
  `wait` won't line up with a bounce. Documented, not solved.
- `BOUNCE_BLOCKS = 8` is a guess; the implementer reports the real speed on
  the dev machine, and the planner can tune it editorially.

## Size
S: ~120 lines plus tests, on top of RECb.

## Changelog
- 2026-10-24: first draft (planner PJ).
