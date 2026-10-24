---
id: REQ-041
type: question
from: planner (PB-audio-files / TRK)
tasks: [TRK]
blocks: []
status: open
created: 2026-10-24
---

## Request
Confirm the semantics of `drop <track>`, especially the forced form.

## Context
The brief: "`drop <track>`: rejected while a Voice uses the track (or forced
with those Voices stopped). Freed through the garbage queue." Voices hold
their own `Arc` clone of the track's samples (README §3.4), so dropping
the track from the engine is memory-safe either way; the question is what
the user sees. A merely *stopped* voice still holds the samples and can be
restarted, so "stop" would free nothing and leave voices whose track name
no longer exists.

## Options
1. **`drop <track>` refuses while any voice (top-level or in a group) uses
   the track and names those voices; `drop -f <track>` unloads them and the
   track in one atomic `Batch`.** Memory is freed once the garbage is
   drained. (Recommended)
2. **`-f` stops the voices but keeps them**: matches the brief's wording
   literally, but frees nothing until those voices are unloaded, and leaves
   voices pointing at a dropped track name.
3. **Lazy drop**: `drop` always succeeds and only removes the name; voices
   keep playing their samples until unloaded. Simplest, but memory
   lingers invisibly.

## Impact
TRK step 5 and three tests (`drop_used_track_is_refused`,
`drop_force_unloads_voices`, `drop_then_import_reuses_id`). Option 1 needs
REQ-039 items 3-4 (listing voices by track, unloading one voice from TRK).
No interface change outside TRK.

## Architect review

## Decision
