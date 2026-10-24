# Project status (PM-owned)

**Phase:** 1 (planning)
**Updated:** 2026-10-24

**Resumed 2026-10-24 (new PM session).** Corrections vs. the previous board: request numbering is consistent (PA uses REQ-024..030, PB REQ-031..041; plan cross-refs checked). Reserved placeholder request files still to be written: REQ-015..017 (PH), 018..023 (PI), 025..030 (PA), 031..041 (PB). Plans on disk that the old board missed: C1, F1a/F1b, GRPa/b, TMPa/b, PRCa/b, VOXa/b, LFO, ENV, EUC, FX (all draft). Architect journal has no plan reviews recorded yet.

Phases: 0 not started · 1 planning · 2 wave 1 · 3 wave 2 · 4 wave 3 · 5 wave 4 · 6 done

## Tasks

| Task | Brief | Wave | Plan | Implementation | Notes |
|---|---|---|---|---|---|
| F1 | PB | 1 | awaiting-user (F1a,b; arch: approve w/ fixes) | none | |
| C1 | PA | 1 | awaiting-user (arch: approve w/ fixes) | none | |
| E1 | PA | 2 | none | none | expected to split into E1a/b/c |
| LOAD | PB | 2 | draft | none | |
| SH | PF | 2 | reviewed (SHa,b + SH-grammar; arch: approve w/ fixes) | none | grammar needs user approval |
| R1 | PA | 3 | none | none | |
| GRP | PH | 3 | draft (GRPa,b) | none | |
| TMP | PH | 3 | draft (TMPa,b) | none | |
| PRC | PH | 3 | draft (PRCa,b) | none | |
| VOX | PI | 3 | draft (VOXa,b) | none | |
| INS | PH | 3 | none | none | |
| TRK | PB | 3 | draft | none | |
| REC | PJ | 4 | reviewed (RECa,b,c; arch: approve w/ fixes) | none | |
| REPL | PF | 4 | draft (REPLa,b) | none | |
| LFO | PI | 4 | draft | none | |
| ENV | PI | 4 | draft | none | |
| EUC | PI | 4 | draft | none | |
| FX | PI | 4 | draft | none | |

Plan states: none → draft → in-review (architect) → awaiting-user → approved (→ revised).
Implementation states: none → scaffolded → dispatched → in-review (planner) → ready → merged (integrate) → in-dev.

## Waves

| Wave | Branch | Scaffolded | Tasks merged | Merged into dev |
|---|---|---|---|---|
| 1 | integrate/wave-1 | step 0 done (2f97859); C1/F1 registry stubs pending plan approval | 0/2 | no |
| 2 | integrate/wave-2 | no | 0/3 | no |
| 3 | integrate/wave-3 | no | 0/7 | no |
| 4 | integrate/wave-4 | no | 0/6 | no |

**User instruction (2026-10-24):** to save usage, run only the architect and planner PB for now; restart other planners one at a time later. PI was stopped mid-edit of VOXa/LFO/ENV and its journal: on respawn, it must check those edits are complete.

## User queue (presented one at a time, top first)

Wave 1 (sent to the user's phone as one multi-question ask, each answered separately):
1. REQ-024 C1 out-of-ownership call-site edits (arch: option 1)
2. REQ-042 C1 plan approval (arch: approve w/ fixes)
3. REQ-031 riff_fixup pre-pass (arch: option 1 w/ conditions)
4. REQ-032 symphonia id3v2 feature (arch: approve)
5. REQ-033 AAC not gapless, §3.2 wording (arch: option 1)
6. REQ-034 corrupt packets → silence (arch: option 1 + sizing fix)
7. REQ-036 extension whitelist, §3.2 wording (arch: option 1)
8. REQ-043 F1a plan approval
9. REQ-044 F1b plan approval

Later (architect-reviewed, awaiting user): REQ-001, 002 (REC designs), 003, 004 (grammar), 005, 006; plan approvals for SH-grammar/SHa/SHb, RECa/b/c.
Answered by PM: REQ-035 (from PB brief).
FYI for user: architect bumped alsa-sys 0.3.1→0.4.0 to resolve with cpal (README §1.1 clarification).

## Agent roster (this session only; cleared on context reset)

| Agent | Role | Scope | Spawned | State |
|---|---|---|---|---|
| architect | Architect | phase 1 reviews: C1, F1a/b, REC, SH + REQ-001..006, 024, 031..036 | 2026-10-24 (resume) | done; not yet reviewed: LOAD, TRK, REPL, PH/PI plans, REQ-007..010, 037..041 |
| planner-PA | Planner | PA-core: E1 plans, R1 plan, REQ-025..030 | 2026-10-24 (resume) | stopped (user: save usage); respawn later, one at a time |
| planner-PB | Planner | PB-audio-files: REQ-031..036 first, then LOAD, TRK, REQ-037..041 | 2026-10-24 (resume) | done (F1a/b, LOAD, TRK; REQ-031..041 all written, open) |
| planner-PF | Planner | PF-shell-repl | n/a | not respawned (planning done; waits for decisions) |
| planner-PH | Planner | PH-engine-commands: REQ-015..017, INS plan | 2026-10-24 (resume) | stopped (user: save usage); respawn later, one at a time |
| planner-PI | Planner | PI-voice-dsp: REQ-018..023 | 2026-10-24 (resume) | stopped (user: save usage); respawn later, one at a time |
| planner-PJ | Planner | PJ-recording | n/a | not respawned (planning done; waits for decisions) |
