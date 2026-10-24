---
id: REQ-014
type: question
from: planner (PH-engine-commands)
tasks: [TMPa, TMPb]
blocks: [TMPb]
status: open
created: 2026-10-24
---

## Request
Decide the user-visible rules for changing and reassigning tempos: what
`assign` moves along with its target, and what `tempo -v <voice>` does when
the voice uses a shared tempo.

## Context
`plans/detailed/TMPa.md` ("Phase rule", step 4) and `plans/detailed/TMPb.md`
("Behaviour"). Today objects refer to tempos in five ways: own, a named
context (`-t c:clk`), the group's (adopted when grouped), the voice's
(`seq -t v`), or a named group's (`-t g:<g>`). Retiming keeps the beat
position in every option (TMPa's anchor model); that part isn't a choice.

## Options
1. **Recommended set**:
   - Syntax: `tempo -t <tc>|-v <voice>|-g <group>|-p <proc> <unit:interval>`
     (retime), `assign -v|-g|-p <target> c:<tc>|g:<group>|v|<unit:interval>`,
     `tc --rm <name>`. Units also accept `bpm`, `ms`, `smp`.
   - `tempo -v kick …` changes only a tempo kick *owns*. If kick follows a
     context or its group, it's refused with a hint (`tempo -t clk …` to
     change the shared one, or `assign -v kick b:120` to give kick its own).
   - `assign` moves the target **and its followers** (seqs with `-t v`;
     group members that adopted the group tempo; objects made with
     `-t g:<g>`). Objects that merely share a context are not moved.
   - The target's old own tempo is freed when nothing uses it.
   - `tc --rm` is refused while anything uses the context, listing the users.
2. **`tempo -v` on a shared tempo changes the shared tempo**: fewer
   refusals, but retiming one voice silently retimes everything on that
   context.
3. **`assign` moves only the target**: simpler to explain, but a `seq -t v`
   no longer follows its voice after the voice is reassigned, which breaks
   what `-t v` promised.
4. **`tc --rm --force`** reassigning users to their own copies: convenient,
   more code; proposed as a follow-up instead.

## Impact
TMPa (one refusal path), TMPb (follower walk). Option 2 removes a check;
option 3 removes the follower walk but changes `-t v` semantics.

## Architect review

## Decision
