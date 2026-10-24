---
id: REQ-001
type: architecture
from: planner (PJ-recording, REC)
tasks: [RECa, RECb, RECc, E1, R1]
blocks: [RECb, RECc]
status: awaiting-user
created: 2026-10-24
---

## Request
Add the recording pipeline to README §3 as an approved contract: a recording
`rtrb` ring (f32) per take, one writer thread per take, a `Command` that carries
the ring's producer into the engine, a `Garbage` variant that brings it back, three
`Event` variants, and two small REPL-side hooks (a notifier and a shutdown join).

## Context
The brief (`briefs/PJ-recording.md`) prescribes the ring and the writer thread,
and README §1.1 already lists `rtrb` "recording". But README §2's architecture
rule says a new thread or a new queue is architecture-weight, and §3 has no
recording section. Plans: `detailed/RECa.md` (writer), `detailed/RECb.md`
(tap, commands, handler).

Proposed §3.10 text (short form):
- `export::Recorder::start(path, RecSpec) -> Result<(rtrb::Producer<f32>, RecHandle), RecError>`
  runs on the REPL thread: creates the file (errors are reported synchronously),
  allocates a ring of `RING_SECONDS = 4` s × rate × channels f32, and spawns a
  detached writer thread named `blast-rec`.
- `Command::RecStart(RecStartArgs { producer })` / `Command::RecStop(RecStopArgs {})`.
  The engine stores the producer in `Conductor.rec: RecTap`; `render` pushes each
  rendered (unclamped) block with `push_entire_slice`, or drops the whole block
  if the ring lacks room (never blocks, never allocates).
- On stop (or when the writer died, seen via `Producer::is_abandoned`), the
  producer leaves through `Garbage::Recorder(Producer<f32>)`. If the garbage
  queue is full, the tap keeps it in a `retiring` slot and retries each period.
- End-of-stream is signalled only by the producer being dropped on the REPL
  side (`Consumer::is_abandoned`); the writer then drains what's left, finalises
  and exits. A `stop` flag (set by the shutdown hook) is the fallback when the
  Conductor is never dropped.
- Events (all `Copy`): `RecDropped { samples: u32 }` (one per overflow episode,
  pushed when the episode ends or the take stops), `RecAborted` (writer gone),
  `BounceDone` (RECc only).
- Hooks: `export::set_notifier(fn(&str))`, called once by the runtime at
  startup with its thread-safe line printer, so the writer can report
  "wrote x.wav (3:02, 69 MB)" or I/O errors; and `export::finish_all(timeout)`,
  called by the runtime's quit path after the audio stream is dropped, so takes
  are finalised before the process exits.

## Options
1. **Adopt as §3.10, as above.** (Recommended) Cost: none beyond what the
   brief already asks for; E1 and R1 learn the names now and the wave-4
   scaffold has exact stubs.
2. **Writer thread reports through the engine's event queue.** Not possible
   without a second producer on that SPSC queue; rejected.
3. **No notifier; results only shown by `rec` (status).** Cheaper by one
   registry line, but I/O errors (disk full) go unnoticed until the user asks.

## Impact
- E1: its `Garbage` enum gains `Recorder(rtrb::Producer<f32>)` and `Event` gains
  three variants (added by the architect in the wave-4 scaffold, but E1 should
  leave room: `Garbage` must allow non-`Copy` payloads; a full garbage queue
  must hand the value back, e.g. `fn discard(&mut self, g) -> Result<(), Garbage>`).
- R1: needs a line printer callable from any non-audio thread, and a quit path
  that drops the cpal stream before exiting, so `finish_all` can run.
- If the mirror implements `Batch` rollback by snapshotting `EngineState`, the
  REC mirror field must be `Clone` (`RecRepr` holds `Arc`s; planned that way).

## Architect review
**Architect, 2026-10-24: design approved (option 1, with four amendments).
Full text: [`design/REQ-001.md`](../design/REQ-001.md).**

The amendments:
1. RECb's last resort is `std::mem::forget` (a bounded leak), never a drop
   on the audio thread.
2. No test calls `export::finish_all`, because it is process-global.
3. `Command` stays move-only (`Send`, not `Clone`/`Debug`).
4. The sample clock advances only inside `render`.

Once approved, I add §3.10 to README §3 and a sentence to §3.4 (the garbage
push hands the value back when the queue is full, and payloads may be
non-`Copy`). I also add the matching E1 and R1 bullets to
`briefs/PA-core.md`. Exact texts are in the design.

## Decision
