# Planner journal: PI-voice-dsp

## Current job
Job 1 (plan), phase 1 (ORG §4.3). Started 2026-10-24; respawned after a
context clear the same day. All six drafts are on disk and complete (every
template section present). Requests REQ-018..023 written. Waiting on the
architect review and on E1's plan (planner PA, in progress).

## Plans
| Plan | Status | Notes |
|---|---|---|
| `detailed/VOXa.md` | draft, complete | wave 3. Param model, mod bank, regions/loops/slices, reverse, pan, pitch, commands. Size L. Retrigger line ref corrected to `processes.rs:166-171` (editorial). |
| `detailed/VOXb.md` | draft, complete | wave 3, after VOXa, same files. Hermite, gain/pan ramps, retrigger declick, start/stop/pause fades. Size M. |
| `detailed/LFO.md` | draft, complete | wave 4. Added `set_enabled` step and `-g <group>` owner form (REQ-020, editorial in draft). Size M. |
| `detailed/ENV.md` | draft, complete | wave 4. Trigger hook = VOXa's `trigger_count` (no registry entry). Added `set_enabled` step and `-g` owner form. Size M. |
| `detailed/EUC.md` | draft, complete | wave 4. Command-side only, in `commands/procs/seq.rs`. Size S. |
| `detailed/FX.md` | draft, complete | wave 4. `engine/fx/**`, hook in `Voice::render_frame`. Size L. |

## Open requests
| ID | Type | Subject | Blocks |
|---|---|---|---|
| REQ-018 | architecture | modulation layer on `VoiceParam` (extends README §3.9) | VOXa steps 2-3, LFO, ENV, FX |
| REQ-019 | question | E1 surface PI plans assume (overlaps REQ-011, REQ-028) | VOXa, LFO, ENV, FX |
| REQ-020 | question | PRC's process API for LFO/ENV/EUC (helpers PRC must expose) | LFO, ENV, EUC |
| REQ-021 | question | interpolation: cubic Hermite vs linear vs sinc | VOXb step 1 |
| REQ-022 | question | declick/ramp scope (VOX vs ENV); stop/pause become short fades | VOXb steps 2-4 |
| REQ-023 | question | Pitch as its own VoiceParam vs sugar over Rate | VOXa (Pitch row, step 6) |

## Cross-plan notes
- PRCa/PRCb (planner PH) agree with us on: `ProcOwner`, `resolve_owner`,
  `parse_seed`/`pick_seed`, `--seed` in `try_seq`, `set_enabled` on every
  process. Gaps (generic engine insert, mirror registration helper, shared
  `-t` parsing, owner parsing incl. `-g`) are in REQ-020.
- E1 plan not on disk yet. PA's REQ-025..029 are placeholders; REQ-028
  (process contract) is the one to read when it lands.

## Implementers
| Task | Agent | Branch | State |
|---|---|---|---|

## Review notes
