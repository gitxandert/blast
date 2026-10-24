# Planner journal: PB-audio-files

## Current job
Phase 1 planning (ORG §4.3) DONE 2026-10-24: F1a, F1b, LOAD, TRK drafted; REQ-031..041 written. Waiting for architect review / PM relays (Job 2).

## Plans
- F1a.md: draft written.
- F1b.md: draft written (checked complete on respawn 2026-10-24).
- LOAD.md: draft written 2026-10-24 (REQ-037, REQ-038 written).
- TRK.md: draft written 2026-10-24 (REQ-039..041). E1/R1 names assumed (REQ-039); refresh in wave 3.
- F1a: editorial fix 2026-10-24 (wave-1 line refs, step-0 helpers i16_to_f32/f32_to_i16).
- LOAD adds `conform()` (used by TRK import).

## Open requests
All open, written 2026-10-24: 031 symphonia regressions (riff_fixup), 032 id3v2 feature (dependency), 033 AAC not gapless, 034 corrupt packets, 035 F1 edits load_assets (blocks F1a), 036 extension whitelist (plan-change), 037 subfolder naming (user decision; blocks LOAD), 038 load wrapper name clash (blocks LOAD; scaffold renames main.rs call to load_assets_dir), 039 TRK E1/R1 interfaces (blocks TRK), 040 import thread (rec: calling thread), 041 drop semantics (rec: refuse; -f unloads).
Cross-planner: REQ-030 (PA, R1 config chooser) may move LOAD's preferred_config; REQ-008 (PF, shared submitter) affects REQ-040 cost.

## Implementers
| Task | Agent | Branch | State |
|---|---|---|---|

## Review notes

## Research notes (phase 1)
- Scratch crate: scratchpad/symprobe (symphonia 0.6.1 approved features + rubato 5.0.1). Measured: MP3 gapless exact; M4A/ADTS AAC not trimmed (67,584 vs 66,150); ADPCM ignores fact (67,122); 3-min MP3 decode 0.25 s release; ID3v2 >1MB fails probe without id3v2 feature.
- rubato default-features=false has no Fft; Async::new_sinc + process_all on InterleavedSlice. 3-min stereo 44.1->48k: 1.5 s (sinc_len 128), 2.4 s (256). Residual sub-sample delay 0.2-1.0 frames; error after alignment ~3e-4.
