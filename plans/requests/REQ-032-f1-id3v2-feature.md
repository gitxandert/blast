---
id: REQ-032
type: dependency
from: planner (PB-audio-files / F1a)
tasks: [F1a]
blocks: []
status: awaiting-user
created: 2026-10-24
---

## Request
Add `"id3v2"` to symphonia's feature list in `blast/Cargo.toml` (README §1.1).
No new crates.

## Context
Without the `id3v2` feature, symphonia's MP3 reader has no ID3v2 parser and
falls back to scanning for a frame sync; it gives up after about 1 MB. MP3s
with embedded cover art often carry a larger tag. Measured: a 2.7 MB MP3 with
a large ID3v2 tag fails to probe without the feature and decodes with it.
Small tags work either way.

Cost: the feature only enables code in `symphonia-metadata`, which is
already in the dependency tree (its deps `lazy_static`, `log`, `regex-lite`,
`smallvec` are already pulled in). Transitive crate count unchanged.
`symphonia-metadata` declares `#![forbid(unsafe_code)]`. Build time change is
within noise (one extra module in a crate that is already compiled).

## Options
1. **Add `id3v2`** (architect edits `Cargo.toml` features; F1 writes no code
   for it). (Recommended)
2. **Leave it out.** MP3s with large tags are skipped with a decode error.

## Impact
README §1.1 symphonia row (features list). `Cargo.toml` is architect-owned
(scaffold). F1a's Registry entries section mentions it. No interface change.

## Architect review
**Architect, 2026-10-24: recommend approval.** Verified independently:

- In symphonia 0.6.1, `id3v2 = ["symphonia-metadata/id3v2"]`. It gates the
  `Id3v2Reader` that `default::get_probe()` registers. symphonia depends on
  `symphonia-metadata` with `default-features = false`, so today the reader
  is off. `symphonia-metadata` declares `#![forbid(unsafe_code)]`.
- I applied the feature in a scratch copy of `integrate/wave-1` @ `2f97859`.
  `Cargo.lock` came out byte-identical, and the normal dependency graph has 67
  crates before and after. No new crates, licenses or versions, so the
  2-week rule and `deny.toml` are unaffected.

If the user approves, I add `"id3v2"` to the symphonia features in
`blast/Cargo.toml` on `integrate/wave-1` as part of wave-1 scaffolding,
before F1a is dispatched. I also update the symphonia row in README §1.1.
F1 writes no code for it.

## Decision
