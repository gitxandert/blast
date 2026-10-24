# Planner journal: PB-audio-files

## Current job
Phase 2 (wave 1) started 2026-10-24. Job 3 for F1a: implementer prompt ready (below), NOT dispatched. Wait for PM's "integrate/wave-1 scaffolded" message, then spawn (general-purpose, isolation worktree, effort medium). F1b dispatches after F1a passes review (branch task/F1b from task/F1a tip).
Phase 1: F1a, F1b, LOAD, TRK drafted; REQ-031..041 written (2026-10-24).

## Plans
- F1a.md: APPROVED (REQ-043) 2026-10-24; architect fixes applied, decisions REQ-031..036 folded in.
- F1b.md: APPROVED (REQ-044) 2026-10-24; architect fixes applied.
- LOAD.md: draft written 2026-10-24 (REQ-037, REQ-038 written).
- TRK.md: draft written 2026-10-24 (REQ-039..041). E1/R1 names assumed (REQ-039); refresh in wave 3.
- F1a: editorial fix 2026-10-24 (wave-1 line refs, step-0 helpers i16_to_f32/f32_to_i16).
- LOAD adds `conform()` (used by TRK import).

## Open requests
Decided 2026-10-24: 031 opt1 (4 conditions), 032 approved (architect adds id3v2 at scaffold), 033 opt1, 034 opt1 (silence-sizing fix), 035 answered (dispatch only), 036 opt1. Still open: 037..041. Original summary: 031 symphonia regressions (riff_fixup), 032 id3v2 feature (dependency), 033 AAC not gapless, 034 corrupt packets, 035 F1 edits load_assets (blocks F1a), 036 extension whitelist (plan-change), 037 subfolder naming (user decision; blocks LOAD), 038 load wrapper name clash (blocks LOAD; scaffold renames main.rs call to load_assets_dir), 039 TRK E1/R1 interfaces (blocks TRK), 040 import thread (rec: calling thread), 041 drop semantics (rec: refuse; -f unloads).
Cross-planner: REQ-030 (PA, R1 config chooser) may move LOAD's preferred_config; REQ-008 (PF, shared submitter) affects REQ-040 cost.

## F1a implementer prompt (ready; dispatch when wave-1 scaffold is done)
Read `/home/alexander-toth/rust_proj/blast/plans/ORG.md` and `/home/alexander-toth/rust_proj/blast/plans/roles/implementer.md`. You implement `/home/alexander-toth/rust_proj/blast/plans/detailed/F1a.md`. Base branch: `integrate/wave-1`. Task branch: `task/F1a`.
Notes: all plans/ paths are under /home/alexander-toth/rust_proj/blast/plans/ (your worktree's copy is stale; never edit plans/). The plan is approved; REQ-031..036 are decided as listed at its top (riff_fixup is in scope; id3v2 is added to Cargo.toml by the architect's scaffold, not by you). Line refs are for 2f97859; re-check them on the scaffolded branch. Read symphonia 0.6.1 from ~/.cargo/registry/src/*/symphonia-*-0.6.1/, not from memory. Tests that use F1b fixtures (`corrupt_flac_frame_is_filled`) must skip when `blast/assets/.fixtures/` is missing. In your report, include the REQ-031 condition 4 list (cases still dropped, error-kind changes) and the measured "today" result of every test-table row. Report to me (planner PB-audio-files) when done or blocked.

## Implementers
| Task | Agent | Branch | State |
|---|---|---|---|

## Review notes

## Research notes (phase 1)
- Scratch crate: scratchpad/symprobe (symphonia 0.6.1 approved features + rubato 5.0.1). Measured: MP3 gapless exact; M4A/ADTS AAC not trimmed (67,584 vs 66,150); ADPCM ignores fact (67,122); 3-min MP3 decode 0.25 s release; ID3v2 >1MB fails probe without id3v2 feature.
- rubato default-features=false has no Fft; Async::new_sinc + process_all on InterleavedSlice. 3-min stereo 44.1->48k: 1.5 s (sinc_len 128), 2.4 s (256). Residual sub-sample delay 0.2-1.0 frames; error after alignment ~3e-4.
