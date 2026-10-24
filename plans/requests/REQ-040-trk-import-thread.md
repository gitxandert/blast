---
id: REQ-040
type: question
from: planner (PB-audio-files / TRK)
tasks: [TRK]
blocks: []
status: open
created: 2026-10-24
---

## Request
Where does `import` decode, resample and remap: on the thread that submitted
the command (REPL or script thread), or on a separate loader thread with a
completion step? The brief asks for a recommendation.

## Context
Measured (release, symphonia 0.6.1 + rubato 5.0.1, scratch crate): a
3-minute 44.1 kHz stereo MP3 decodes in 0.25 s; resampling it to 48 kHz
with LOAD's parameters takes about 2.4 s. A file already at the engine rate
needs only the decode. Typical samples (under 10 s) take a few ms.
Scripts run on their own thread (README §3.7), and REQ-008 (open) proposes
sharing `CmdProcessor` between the REPL and script threads behind a mutex.

## Options
1. **Decode on the calling thread** inside `try_import`. Deterministic:
   `import x; load x` works in a script with no waiting. No new thread,
   channel or REPL polling. Cost: interactive typing freezes for the
   duration (about 2.7 s for a long off-rate MP3; the redraw thread keeps
   the prompt marker animated); with REQ-008 option 1, a script's long
   `import` holds the submitter lock and freezes typing too. (Recommended:
   long imports are rare in a sampler, and the brief's "loading…" note can
   be added if REQ-039 item 10 gives commands a way to print.)
2. **Loader thread per import** (`std::thread::spawn`, no new crate):
   `import` reserves the name in the mirror as "loading", returns at once
   with "loading 'x'…", and the worker decodes and conforms the file. On
   completion it inserts the `TrackRepr` and submits `AddTrack`, either by
   locking the shared submitter (if REQ-008 option 1) or by sending the
   result to the REPL thread, which then has to poll for it. `load x` while
   loading fails with "still loading", so scripts need a way to wait for an
   import. This is an architecture item (new thread, a completion path into
   the mirror).

## Impact
Option 1: TRK step 3 as planned. Option 2: TRK grows a loader module, a
"loading" state on `TrackRepr`, a completion hook in R1/REPL's loop (or in
REQ-008's submitter), and SH/REPL need a `wait` on imports; README §3
would list the loader thread.

## Architect review

## Decision
