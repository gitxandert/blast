---
id: REQ-035
type: question
from: planner (PB-audio-files / F1a)
tasks: [F1a, LOAD]
blocks: [F1a]
status: answered
created: 2026-10-24
---

## Request
Confirm that F1a may edit the format dispatch inside `load_assets`
(`file_parsing/mod.rs`, the `match ext.as_str()` arm) in wave 1, although
README §2 excludes `load_assets` from F1's ownership.

## Context
README §2 wave 1: F1 owns "`file_parsing/**` except `load_assets` (it keeps
working unchanged)". The brief says "`load_assets` keeps its signature and
behaviour; it just calls `decode_file`." Today `load_assets` calls
`wav::parse_file`/`aiff::parse_file` directly by extension
(`mod.rs:40-48` on `dev`), and F1 deletes those modules, so `load_assets`
cannot keep compiling without a change.

## Options
1. **F1a replaces only the dispatch match** with `is_supported_extension` +
   `decode_file`. Signature, sorting, duplicate handling, rate vote and
   rate-mismatch filter stay as they are (LOAD rewrites them in wave 2). The
   README §2 row is read as "behaviour unchanged". (Recommended)
2. **Keep `wav.rs`/`aiff.rs` alive until LOAD** in wave 2: two decoders
   coexist for a wave, against the brief.

## Impact
README §2 wave 1 ownership wording (architect may want to adjust it). F1a
step 4. LOAD starts from the edited function. No interface change.

## Architect review
**Architect, 2026-10-24: option 1. The approved docs already answer this.**

The PB brief (architect-owned, approved) says "`load_assets` keeps its
signature and behaviour; it just calls `decode_file`." The README §2 phrase
"except `load_assets` (it keeps working unchanged)" was meant as "LOAD owns
its logic, and its behaviour doesn't change in wave 1". It wasn't meant to
forbid the dispatch edit, which is forced once `wav.rs`/`aiff.rs` are
deleted. I've clarified the README §2 wave-1 F1 row to say so (no change in
meaning; logged in `state/architect.md`).

F1a changes only the extension/decoder dispatch. Signature, sorting,
duplicate handling, the rate vote and the rate filter stay. LOAD starts from
the edited function in wave 2. The PM can close this as `answered`, citing
the brief.

## Decision
PM, 2026-10-24: **answered** from approved docs, no user decision needed (ORG §3). The PB brief (F1) says "`load_assets` keeps its signature and behaviour; it just calls `decode_file`." F1a may edit only the decoder dispatch inside `load_assets`. README §2 F1 row clarified by the architect to match (no change in meaning).
