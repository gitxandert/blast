---
id: REQ-006
type: question
from: planner (PF-shell-repl, SH)
tasks: [SH, INS, GRP, VOX, PRC, TMP]
blocks: []
status: open
created: 2026-10-24
---

## Request
Confirm that the `all` reserved word (notes.txt) is expanded by the command
layer, not the shell, and say which task implements it.

## Context
The brief asks whether the shell expands `all` by asking
`CmdProcessor::names` (INS) through a trait, or whether `all` belongs to the
command layer. A script is expanded by the shell before its earlier lines
have run (`detailed/SH-grammar.md` §0 and §6). So `load kick; start -v all`
would expand `all` before `kick` exists. Only the command layer knows the
names at the moment a line runs. It also keeps the shell free of engine
knowledge (README §3.7). The shell only reserves the word (no variable or
function may be called `all`) and passes it through.

## Options
1. **Command layer; the shell passes `all` through.** Each command that
   takes a voice/group target accepts `all`; a shared helper in
   `commands/` (e.g. `fn targets(&self, kind, name) -> Vec<Id>`) does the
   expansion, built into a `Command::Batch` or a multi-target command.
   Suggested owner: INS (it already builds `names`), or whichever PH task the
   PM picks; it touches the `try_*` of several families, so it may need a
   wave-3 registry entry. (Recommended.)
2. **Shell expands it** via a `NameSource` trait implemented by
   `CmdProcessor`. Wrong for scripts (stale names) and couples the shell to
   the command layer.
3. **Defer `all`** entirely (nobody implements it in this roadmap).

## Impact
Option 1 adds a small item to one PH task (wave 3) and nothing to SH.
Option 2 adds a trait and engine knowledge to SH and is subtly wrong for
scripts. Option 3 leaves a notes.txt item undone.

## Architect review

## Decision
