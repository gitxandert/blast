---
id: REQ-007
type: architecture
from: planner (PF-shell-repl, REPL)
tasks: [REPLb, E1, C1, R1]
blocks: [REPLb]
status: open
created: 2026-10-24
---

## Request
Add to the §3.5 command-layer API a way to submit several command lines as
one `Command::Batch`, all-or-nothing on the mirror, and assign it to a task
that owns `commands/**` before wave 4.

## Context
The shell emits `ShellOut::Batch(Vec<String>)` for `a & b` and named blocks
(`detailed/SH-grammar.md` §4). README §3.4 has `Command::Batch(Vec<Command>)`
applied atomically by the engine, and §3.5 has `CmdProcessor::submit` for one
line, with the invariant that a failed parse or a full queue leaves the
mirror untouched. For a batch, line 2 may depend on line 1 (`load kick &
seq kick …`), so lines must be parsed in order against a mirror that already
includes the earlier lines. If line 3 then fails, lines 1–2's mirror
changes must be undone. REPL can't do this: `commands/**` isn't REPL's, and
the rollback needs the mirror's internals. Proposed signature:
```rust
impl CmdProcessor {
    /// Parse every line in order; on success push one Command::Batch.
    /// On any parse error or a full queue, the mirror is exactly as before.
    pub fn submit_batch(&mut self, lines: &[String], queue: &mut CmdSender)
        -> Result<Option<String>, CmdErr>;   // Local outputs joined by newlines
}
```
Simplest correct implementation: clone `EngineState` before parsing and
restore it on failure (the mirror is small, plain data after E1, and this
runs on the REPL thread). A `Local` command (e.g. `ls`) inside a batch
returns its text and adds nothing to the batch.

## Options
1. **E1 adds `submit_batch`** in wave 2 (E1 introduces `Command::Batch` and
   owns `commands/**`). (Recommended: the batch, the mirror and the queue
   change together there.)
2. **R1 adds it** in wave 3 when it changes `submit` to take `&mut CmdSender`
   (R1 owns `commands/queue.rs` only, so this needs a registry entry in
   `commands/mod.rs`).
3. **REPL owns a one-function region of `commands/mod.rs`** in wave 4
   (an ownership change).

## Impact
Without it, `&` and named blocks can't be atomic. Option 1 adds a small step
and a test (`failed_batch_leaves_mirror_unchanged`) to E1's plan (planner PA).
Options 2/3 move it later or across ownership lines.

## Architect review

## Decision
