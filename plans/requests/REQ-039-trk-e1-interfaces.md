---
id: REQ-039
type: question
from: planner (PB-audio-files / TRK)
tasks: [TRK, E1a, E1b, E1c, R1]
blocks: [TRK]
status: open
created: 2026-10-24
---

## Request
Confirm (or correct) the E1 and R1 surface that `detailed/TRK.md` is written
against. E1's and R1's plans didn't exist when TRK was planned. Where E1/R1
pick other names, TRK adopts them in its wave-3 refresh (editorial). Items
marked **needed** are behaviours TRK relies on; if E1/R1 won't provide one,
TRK needs a plan change.

## Context
TRK (wave 3) adds `import`, `rescan` and `drop` on top of E1 (wave 2) and
alongside R1 (wave 3). Same pattern as REQ-011 (PH) and REQ-019 (PI); items
overlapping REQ-011 (ID allocator, garbage queue, `Batch`) are consistent
with it. Routed to the architect for cross-plan consistency, and to the PA
planner.

## Assumptions

**Mirror (E1, `commands/**`)**
1. **needed** `TrackId` (`Copy`, `Eq`, `Hash`, `Debug`) with
   `EngineState::{alloc_track, free_track}` on the shared allocator, a
   track name index, and an arena-full error returned before any queueing
   (`Limits::tracks`). A count of free track slots (for `rescan`'s
   all-or-nothing check).
2. **needed** `TrackRepr` lives in `commands/track.rs` (C1's layout) with
   `pub(crate)` fields `id`, `name`, `channels`, `frames`; TRK adds
   `source: Option<PathBuf>`.
3. **needed** `VoiceRepr.track: TrackId` readable, plus a way to list every
   voice that uses a track, top-level and group members, with their display
   names (`kick`, `g.kick`).
4. **needed** The mirror-side unload of a single voice (top-level or group
   member) callable from TRK, returning the `Command` that unloads it and
   updating the mirror (e.g. `EngineState::unload_voice(VoiceId) ->
   CmdResult<Command>`), usable inside a `Command::Batch`.
5. **needed** Startup builds the mirror's tracks and the engine's track
   arena from one ordered `Vec<AudioFile>` (IDs allocated in order), with
   no `HashMap` iteration deciding an index on either side (brief: the old
   `Conductor::prepare`/`EngineState::new` coupling must be gone).

**Engine (E1, `engine/**`)**
6. **needed** `Conductor.tracks: Arena<Track>` with
   `Track { samples: Arc<[f32]>, channels: u32 }` (insert/remove by ID, no
   allocation), Voices cloning the `Arc` at load, and a garbage enum with a
   variant that carries `Arc<[f32]>` (else the architect adds
   `Garbage::Track` as a TRK registry entry), plus E1's push helper and its
   full-queue policy.
7. **needed** `engine/handlers/track.rs` exists after E1 (README brief for
   E1 lists it) and is called from the single `apply` match.
8. `Command::Batch(Vec<Command>)` applies atomically; its `Vec` returns
   through the garbage queue (README §3.4).

**Runtime (R1)**
9. **needed** The mirror can read the engine's output rate and channel
   count (e.g. `EngineState::{sample_rate(), out_channels()}`, or
   `blast_time::sample_rate::get()` as today), and the assets directory
   (`Option<PathBuf>`, set by R1 from `cli::Options::assets_dir` when it
   builds the mirror). Who owns the field: E1 declares it, R1 sets it.

**Command layer (C1/E1)**
10. Nice to have: a way for an engine command to return a note for the REPL
    to print (e.g. `Parsed::Engine` carrying `Option<String>`, or a
    `Parsed::EngineNote(Command, String)` variant), so `import` and
    `rescan` can say what they did, and `rescan` with nothing new can
    answer with text only. Without it, TRK is silent on success.

## Options
1. **Confirm the list as written** (E1/R1 adopt the names or map them 1:1).
   (Recommended)
2. **Correct items**: the architect lists deviations and the PB planner
   revises TRK before plan approval.

## Impact
`detailed/TRK.md` (Interfaces, steps 1-6, Registry entries). Items 3-4 and
6 decide whether `drop -f` and freeing samples work as planned; item 9
needs a field R1 fills.

## Architect review

## Decision
