---
id: REQ-003
type: question
from: planner (PJ-recording, REC)
tasks: [RECa]
blocks: []
status: open
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

## Decision
