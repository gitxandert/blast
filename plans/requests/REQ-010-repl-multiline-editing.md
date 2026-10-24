---
id: REQ-010
type: question
from: planner (PF-shell-repl, REPL)
tasks: [REPLb]
blocks: []
status: open
created: 2026-10-24
---

## Request
The brief asks for a recommendation: is multi-line editing of open shell
blocks in scope for REPL? Recommendation: **no, not in this roadmap.**

## Context
With REPLb, an open block (`kick {`) shows a `..` continuation prompt and
each line is edited on its own when typed. Earlier lines of the block can't
be changed; Ctrl-C drops the whole block; Up recalls single lines from
history. Real multi-line editing (moving the cursor up into earlier lines,
recalling a whole block from history) needs multi-row rendering, which
collides with the planned split-screen view from `notes.txt` (R1 keeps it
possible), and it's a sizeable piece of terminal work. Scripts longer than a
few lines belong in a file run with `source`, which is cheap to re-run.

## Options
1. **Out of scope.** Line-at-a-time continuation only; revisit together with
   the split-screen view. (Recommended.)
2. **In scope, minimal:** store a finished block in history as one entry
   (joined with `; `, so recalling it gives a one-line, editable version).
   Small: about one step in REPLa/REPLb.
3. **In scope, full:** a multi-row editor. Its own task (L), wave 4 or later.

## Impact
Option 1: nothing. Option 2: a small addition to REPLb and a history format
note. Option 3: a new task and plan.

## Architect review

## Decision
