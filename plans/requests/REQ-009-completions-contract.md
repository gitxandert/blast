---
id: REQ-009
type: question
from: planner (PF-shell-repl, REPL)
tasks: [INS, REPLb]
blocks: [REPLb]
status: open
created: 2026-10-24
---

## Request
Pin down the meaning of README §3.8's
`CmdProcessor::completions(&self, line: &str, cursor: usize) -> Vec<String>`
so that REPL (wave 4) and INS (wave 3) agree.

## Context
`detailed/REPLb.md` step 7 assumes:
- `line` is one plain command line (no shell syntax: REPL passes only the
  segment after the last `;`, `&`, `{`, found with `shell::command_start`).
- `cursor` is a **byte offset** into `line`, at a char boundary.
- The result is **whole replacement words** for the word that ends at
  `cursor` (the text after the last whitespace before `cursor`), sorted,
  without duplicates, and not including a trailing space. Example:
  `completions("start -v k", 10)` → `["kick"]`, not `["ick"]`.
- An empty word (cursor after a space) lists every valid next word.
- It never fails and never mutates the mirror (it takes `&self`).

## Options
1. **Adopt the semantics above** and add them to §3.8 and INS's plan.
   (Recommended.)
2. **INS defines them differently** (e.g. returns suffixes, or char
   offsets); REPLb adapts in its refresh before wave 4.

## Impact
INS (planner PH) and REPLb only. A mismatch would be found only in wave 4,
so fixing it now avoids a plan change then.

## Architect review

## Decision
