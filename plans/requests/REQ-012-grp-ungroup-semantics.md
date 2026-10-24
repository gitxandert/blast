---
id: REQ-012
type: question
from: planner (PH-engine-commands)
tasks: [GRPa]
blocks: [GRPa]
status: open
created: 2026-10-24
---

## Request
Decide what `ungroup`, `group <g> --add` and `group --rm` do with names,
processes, tempos and playing state (brief PH: "decide what happens").

## Context
`plans/detailed/GRPa.md`, section "Behaviour". After E1, voices stay in the
voice arena and a group only lists `VoiceId`s (REQ-011 item 9), so
membership edits are bookkeeping. Four sub-decisions; the recommendation is
one coherent set.

## Options
1. **Recommended set**:
   - *Syntax*: `group <g> --add a,b`, `ungroup <g> [a,b]` (no list
     dissolves), `group --rm <g>` (deletes the group and its voices). Same
     flag style as `tc --rm` (TMP) and `proc --rm` (PRCb).
   - *Name clash on ungroup* (a top-level voice already has the name):
     reject the whole command and name the clashes. Nothing changes.
   - *Group-owned processes* on dissolve: removed (they act on the group,
     which no longer exists).
   - *Group tempo* on dissolve/delete: if anything still uses it (ex-members
     that adopted it, voices or seqs created with `-t g:<g>`), it becomes a
     tempo context named `<g>` (or `<g>1`, `<g>2`… if taken), keeping its
     phase. Otherwise it's freed. `tc --rm` can remove it later.
   - *Playing state*: a voice leaving a group keeps what you hear:
     `active = voice active && group active`. A voice joining a paused group
     goes silent until the group starts.
2. **Auto-rename on clash** (`a` → `g_a`): ungroup never fails, but names
   change behind the user's back and scripts break.
3. **Give each follower its own copy of the group tempo** instead of
   promoting: no leftover context, but N tempos instead of one, and the
   ex-members drift apart if one is later retimed.
4. **Followers lose their tempo** (back to "no tempo"): simplest, but seqs
   on those voices stop firing until reassigned.
5. **Generic verbs from notes.txt** (`add a,b g`, `fuse`) instead of
   `group --add`: reads well, but `add` would be a cross-family verb whose
   meaning depends on argument types; better decided together with SH's
   grammar later.

## Impact
GRPa only (`commands/group.rs`, `engine/handlers/group.rs`). Options 2–4
change test expectations, not structure. Option 5 adds a parse arm and
couples to SH (REQ-004).

## Architect review

## Decision
