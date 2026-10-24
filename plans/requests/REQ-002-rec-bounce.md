---
id: REQ-002
type: architecture
from: planner (PJ-recording, REC)
tasks: [RECc]
blocks: [RECc]
status: with-architect
created: 2026-10-24
---

## Request
Choose how `bounce <seconds> <path>` (offline render, faster than real time)
works once the Conductor lives inside the cpal callback (R1). Recommendation:
option (a) in the form below, reusing REC's recording ring and writer.

## Context
Brief `briefs/PJ-recording.md`, last bullet. Plan: `detailed/RECc.md`.
Facts that decide it (current code on `dev`):
- The global clock (`blast_time.rs:24-37`, `SAMPLE_COUNTER`) and
  `sample_rate` (`blast_time.rs:6-18`) are process-wide statics. A second
  Conductor ticking them would corrupt the live engine's time.
- The mirror (`EngineState`) only knows structure (voices, groups, tempos,
  seqs), not run-time state: play positions, tempo phases, RNG state. A
  Conductor rebuilt from it would not sound like the live session.

## Options
1. **(a) Bounce inside the callback, muted.** (Recommended) `Command::Bounce
   { producer, frames }`. While a bounce runs, `Conductor::render` renders up to
   `BOUNCE_BLOCKS = 8` blocks of the requested size per callback, each pushed
   to the recording ring only when it has room for the whole block (so nothing
   is ever dropped; the writer thread's speed sets the pace), stops exactly at
   `frames`, then fills the device buffer with silence. When done it retires the
   producer through the garbage queue and pushes `Event::BounceDone`.
   Semantics the user should accept: the bounce renders *the live session's
   next N seconds*; live output is silent meanwhile; afterwards the session has
   advanced N seconds; commands typed during a bounce apply at the next block
   boundary in bounce time (so scripts with `wait` don't line up). Speed: up to
   8× real time, less on heavy sessions. Cost: about 150 lines in
   `render.rs`/`record` files, no new thread or queue beyond REQ-001 (the
   recording pipeline). Overrunning the callback deadline during a bounce only
   causes inaudible xruns (output is silent anyway).
2. **(b) Second Conductor from the mirror state, on a bounce thread.**
   Needs: the clock and sample rate moved off globals into the Conductor (E1,
   TMP, PRC and every process touch them), a mirror-to-engine rebuild path, and
   it still can't reproduce run-time state. Architecture change to §3.4;
   L–XL across several owners. Not recommended for this roadmap.
3. **(c) Out of scope.** Users record in real time with `rec`. Zero cost.
   Reasonable if the muted-live semantics of (a) are unwanted.

## Impact
(a): registry entries `Command::Bounce`, `Event::BounceDone`, the `bounce`
parse arm and HELP row; RECc runs after RECb in wave 4. (b): reopens E1's
design. (c): RECc is dropped.

## Architect review

## Decision
