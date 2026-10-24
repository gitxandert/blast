# Planner journal: PB-audio-files

## Current job
Phase 1 planning (ORG §4.3): writing detailed/F1*.md, LOAD.md, TRK.md against dev + scaffold step 0. Started 2026-10-24.

## Plans
- F1a.md: draft written.
- F1b.md, LOAD.md, TRK.md: in progress.

## Open requests
Reserved REQ-031..041 (placeholders until written): 031 symphonia regressions, 032 id3v2 feature, 033 AAC not gapless, 034 corrupt packets, 035 F1 edits load_assets, 036 extension whitelist, 037 subfolder naming, 038 load wrapper name clash, 039 TRK E1 interfaces, 040 import thread, 041 drop semantics.

## Implementers
| Task | Agent | Branch | State |
|---|---|---|---|

## Review notes

## Research notes (phase 1)
- Scratch crate: scratchpad/symprobe (symphonia 0.6.1 approved features + rubato 5.0.1). Measured: MP3 gapless exact; M4A/ADTS AAC not trimmed (67,584 vs 66,150); ADPCM ignores fact (67,122); 3-min MP3 decode 0.25 s release; ID3v2 >1MB fails probe without id3v2 feature.
- rubato default-features=false has no Fft; Async::new_sinc + process_all on InterleavedSlice. 3-min stereo 44.1->48k: 1.5 s (sinc_len 128), 2.4 s (256). Residual sub-sample delay 0.2-1.0 frames; error after alignment ~3e-4.
