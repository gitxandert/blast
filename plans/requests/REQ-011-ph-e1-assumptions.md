---
id: REQ-011
type: question
from: planner (PH-engine-commands)
tasks: [GRPa, GRPb, TMP, PRCa, PRCb, PRCc, INS, E1a, E1b, E1c]
blocks: [GRPa, GRPb, TMP, PRCa, PRCb, PRCc, INS]
status: open
created: 2026-10-24
---

## Request
Confirm (or correct) the E1 surface that the PH plans are written against.
E1's detailed plan didn't exist when the PH plans were written, so they
build on README §3.4/§3.5 plus the assumptions below. Wherever E1 picks
different names, the PH plans take E1's names in their wave-3 refresh
(editorial). Anything marked **needed** is a behaviour the PH tasks rely on;
if E1 won't provide it, the PH plans need a plan change.

## Context
PH tasks (wave 3) add commands on top of E1 (wave 2). The planner for E1 is
PA-core, writing in parallel. Routed to the architect for cross-plan
consistency, and to the PA planner.

## Assumptions

**Mirror (`commands/**`)**
1. **needed** ID newtypes `VoiceId`, `GroupId`, `TempoId`, `ProcId`, `TrackId`
   (`Copy`, `Eq`, `Hash`, `Debug`), allocated and freed through one allocator
   on `EngineState` (`alloc_*` / `free_*`). Freed IDs are reusable.
2. **needed** Each `*Repr` is reachable by ID, and by name through a name
   index. Group members are named `group.voice`; top-level voices by their bare
   name. Voices and groups are separate name spaces (as today).
3. **needed** Repr fields that other families read are `pub(crate)` (or have
   `pub(crate)` getters). PH tasks *mutate* fields of Reprs owned by other
   wave-3 tasks only where the plan says so (e.g. GRP sets `VoiceRepr.group`).
4. **needed** The mirror records *why* an object uses a tempo, not just which
   `TempoId`: own tempo, a named context, its group's tempo (adopted when
   grouped), its voice's tempo (`seq -t v`), or a named group's tempo
   (`-t g:<name>`). Today this is `TempoRepr.mode` + `owned`
   (`commands.rs:203-263`). GRP (adoption on `--add`), TMP (`assign` moves
   followers) and INS (`info`) all read it.
5. **needed** A tempo carries an ownership tag (today `TempoMode`), so the
   mirror knows which object a tempo belongs to and the engine knows who
   starts/stops it. GRP changes a group's tag to `Context` when it promotes
   the group's tempo (REQ-012).
6. `ProcRepr` has `owner` (voice or group), `name`, and its tempo reference.
   Process names are unique per owner.
7. `try_*` functions take `&str` and return `CmdResult<Command>`;
   `CmdProcessor::parse` returns `CmdResult<Parsed>` (README §3.5).

**Engine (`engine/**`)**
8. **needed** Slot arenas for voices, groups, tempos, and processes, indexed
   by ID, sized by one `Limits` struct that includes a per-group member
   capacity (`Limits::group_members`).
9. **needed** Group model: voices stay in the voice arena; a `Group` holds a
   preallocated `Vec<VoiceId>` (capacity `group_members`); `Voice.group:
   Option<GroupId>` gates playback (audible = voice active && group active).
   GRP's membership commands are written for this model.
10. **needed** Processes live in a process arena keyed by `ProcId`, each with
    an owner (voice or group) and a `TempoId`. The Conductor can find a
    voice's or group's processes without allocating.
11. **needed** Every active tempo is ticked once per frame by the Conductor
    (README §3.4 recommendation). TMP's phase-continuity design assumes a
    single `update` per frame per tempo.
12. **needed** A garbage queue with a `Garbage` enum covering removed voices,
    groups and processes (and any `Vec` a command carried). Handlers push
    removed objects into it and never drop heap data on the audio thread.
13. `Command::Batch(Vec<Command>)` applies atomically within one period, and
    the `Vec` is returned through the garbage queue. PH builds multi-object
    edits as batches of small `Copy` commands.
14. Handlers are `impl Conductor` blocks in `engine/handlers/<family>.rs`,
    called from one `apply` match (registry).
15. E1's `unload` (voice) removes a voice's processes and its own tempo, and
    is callable as a helper (`Conductor::remove_voice(VoiceId)`) from GRP's
    group handler.
16. `Seq::fire` keeps today's crossing/wrap logic and the `epoch` restart
    check (`processes.rs:96-147`), reading `&TempoState` by ID.
17. `test_util` keeps `setup`/`run` (mirror + engine), `render` returns
    `Vec<f32>`, and there's a helper to drain the garbage queue in tests.

## Options
1. **Confirm the list as written** (E1 plan adopts the names/behaviours, or
   maps them 1:1). (Recommended)
2. **Correct items**: the architect lists the deviations; the PH planner
   revises the affected plans before plan approval.

## Impact
All seven PH plans. Items 4, 8–12 shape the design of GRP and TMP; a
different group model (item 9: voices moved into the group) would rewrite
GRPa's engine half.

## Architect review

## Decision
