---
id: REQ-006
type: question
from: planner (PF-shell-repl, SH)
tasks: [SH, INS, GRP, VOX, PRC, TMP]
blocks: []
status: awaiting-user
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
**Architect, 2026-10-24: option 1. `all` belongs to the command layer.**

Only the command layer knows the names when a line runs. A shell-side `all`
would be stale for scripts, and would couple §3.7 to §3.8.

Proposed split, which avoids cross-ownership edits:
- **E1 (wave 2, owns all of `commands/**`)** adds the helper:
  `EngineState::all_ids(kind) -> Vec<Id>` in `commands/reprs.rs`, ordered
  by ID. It also makes `all` a reserved name in the mirror: creating a
  voice, group or tempo context called `all` (for example `load all` from an
  `all.wav`) fails with a clear error, so `-v all` is never ambiguous.
- **Each wave-3 family task, in its own file,** accepts `all` where its
  commands take a target name, and emits one `Command::Batch`, so it's
  atomic like `&`. VOX handles the transport commands (`start/stop/pause/resume
  -v all`, `velocity`), GRP handles `-g all`, TMP handles `-t all`, and PRC
  handles its process targets.
- **INS** documents `all` in help and offers it in completions.

Each part is small, and nobody edits a file they don't own.

On approval, I add the E1 item to `briefs/PA-core.md` and the per-family items
to `briefs/PH-engine-commands.md` and `briefs/PI-voice-dsp.md` (VOX). Any
wave-3 plans already drafted (GRPa/b, TMPa/b, PRCa/b, VOXa/b) then add an
`all` step, as an editorial-scope addition that the user ratifies with those
plans.

## Decision
