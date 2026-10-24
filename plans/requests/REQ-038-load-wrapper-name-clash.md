---
id: REQ-038
type: question
from: planner (PB-audio-files / LOAD)
tasks: [LOAD, R1]
blocks: [LOAD]
status: open
created: 2026-10-24
---

## Request
README §3.3's new `load_assets(&[AssetInfo], rate, channels)` has the same
name as today's `load_assets(dir)`, which `main.rs` calls and which must keep
working until R1 rewires startup in wave 3. LOAD doesn't own `main.rs`.
Which function gets a new name?

## Context
The brief (LOAD) says "keep the old `load_assets(dir)` working as a wrapper
until R1 rewires startup in wave 3, since LOAD doesn't own `main.rs`". Rust
has no overloading, so both can't be called `load_assets` in one module.
`main.rs:12` (on `integrate/wave-1`) is the only caller outside
`file_parsing`.

## Options
1. **Rename the wrapper** to `load_assets_dir(dir: &str)` (old signature and
   return type). The architect changes the one call in `main.rs` in the
   wave-2 scaffold and adds a compiling stub; LOAD fills it in; R1 deletes
   it in wave 3 when it calls `scan_assets`/`load_assets` directly. The
   contract name stays as README §3.3 says. (Recommended)
2. **Keep the old name for the wrapper** and give the contract function a
   new name (e.g. `load_scanned`). No scaffold edit to `main.rs`, but README
   §3.3 changes and the wrapper name persists past R1 unless R1 renames
   functions in `file_parsing/mod.rs`, which it doesn't own.
3. **Put the new function in a submodule** (`file_parsing::load::load_assets`).
   Adds a module LOAD doesn't own today and splits `file_parsing`'s API.

## Impact
Option 1: one line in `main.rs` plus a stub in `file_parsing/mod.rs` in the
wave-2 scaffold (architect); R1's plan deletes `load_assets_dir`. If E1
also changes `main.rs`/`run_blast` in wave 2, the wrapper's return type
(`HashMap<String, AudioFile>, rate, channels`) stays as today and E1 adapts
the call site. Option 2 changes README §3.3.

## Architect review

## Decision
