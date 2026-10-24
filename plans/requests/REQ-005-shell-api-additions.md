---
id: REQ-005
type: architecture
from: planner (PF-shell-repl, SH)
tasks: [SHb, REPLb]
blocks: [SHb]
status: open
created: 2026-10-24
---

## Request
Record additive items in README §3.7 (Shell): helper methods on `Shell`, the
concrete `Wait` and `ShellErr` types, and `source` handled inside the shell
through a file loader.

## Context
§3.7 fixes `Shell::feed` and `ShellOut` and leaves the rest open. REPL needs
more than `feed` to integrate (continuation prompt, Ctrl-C on an open block,
`--script`, completion). Plans: `detailed/SHb.md` (Interfaces), `detailed/REPLb.md`.
Proposed additions, none changing existing items:
```rust
pub type Loader = Box<dyn FnMut(&Path) -> io::Result<String> + Send>;
impl Shell {
    pub fn new() -> Self;                         // loader = fs::read_to_string
    pub fn with_loader(loader: Loader) -> Self;
    pub fn source(&mut self, path: &str) -> Result<Vec<ShellOut>, ShellErr>;
    pub fn pending(&self) -> bool;                // a block is open
    pub fn reset(&mut self);                      // drop the open block
    pub fn names(&self) -> Vec<String>;           // fns and globals, for completion
}
pub fn command_start(line: &str, cursor: usize) -> usize;
pub enum Wait { Millis(f64), Beats { beats: f64, bpm: f64 } }   // + fn duration()
pub struct ShellErr { pub src: String, pub line: u32, pub col: u32, pub kind: ShellErrKind }
```
**`source` inside the shell.** The shell reads sourced files itself
(through the loader) and expands them inline, so variables and functions
from a sourced file persist, nested sources work, and errors carry the
sourced file's name and line. The shell does file I/O only on the REPL
thread, at `feed` time. Alternative: a `ShellOut::Source(path)` variant that
the REPL resolves, which changes the §3.7 enum and makes nested sources and
error locations harder.

## Options
1. **Approve the additions, with `source` via the loader.** (Recommended.)
2. **Approve the methods but resolve `source` in the REPL** through a new
   `ShellOut::Source(String)`.

## Impact
Only SH and REPL use these. Option 2 changes the §3.7 enum and moves file
reading and source-depth/cycle handling into REPLb.

## Architect review

## Decision
