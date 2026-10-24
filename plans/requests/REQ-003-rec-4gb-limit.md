---
id: REQ-003
type: question
from: planner (PJ-recording, REC)
tasks: [RECa]
blocks: []
status: awaiting-user
created: 2026-10-24
---

## Request
What should a recording do when it reaches the WAV size limit (about 4 GB,
3.1 h of float stereo at 48 kHz)? Recommendation: continue in numbered files.

## Context
hound 3.5.1 keeps the data size in a `u32` (`write.rs:436`,
`data_bytes_written += …`): past 4 GiB it panics in debug builds and writes a
corrupt header in release. It can't write RF64. Plan `detailed/RECa.md`
enforces a frame-aligned per-file cap in the writer thread either way.

## Options
1. **Split into numbered files.** (Recommended) `take.wav`, then
   `take-002.wav`, `take-003.wav`, … at a frame boundary, gapless; the writer
   reports each new file through the notifier. Nothing is lost. Cost: ~30 lines.
2. **Stop the take with a notice.** The file ends at the cap; the engine keeps
   pushing until the producer is retired, so the writer just discards the rest
   and reports `RecAborted`-style. Simpler by ~15 lines; loses audio.
3. Either of the above with a **2 GiB cap** instead of ~4 GiB, for tools that
   read the RIFF size as signed. Costs nothing; files split twice as often.

## Impact
User-visible behaviour of long takes only. RECa's `split_at_size_limit` test
changes with the choice. No other task affected.

## Architect review
**Architect, 2026-10-24: I agree with option 1 (split into numbered files).**

The cap of `u32::MAX - 4096` data bytes is correct for hound's `u32`
counter. It leaves room for the largest header hound writes
(WAVE_FORMAT_EXTENSIBLE), so the RIFF size can't overflow either.

Option 3's 2 GiB cap isn't worth it. Every reader in this project (symphonia,
hound, ffmpeg) treats the size as unsigned, and halving the cap only doubles
the file count.

No architecture impact: it lives in RECa's writer only. It's user-visible
behaviour, so the user decides.

## Decision
