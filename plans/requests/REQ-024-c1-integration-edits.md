---
id: REQ-024
type: question
from: planner (PA-core, C1)
tasks: [C1]
blocks: [C1]
status: awaiting-user
created: 2026-10-24
---

## Request
C1 changes `parse` and `submit` to the README §3.5 signatures. That breaks
seven call sites in files C1 doesn't own. How do those edits land: does the
architect apply them in the C1 merge, or is C1 allowed to make them?

## Context
`detailed/C1.md`, "Registry entries". The call sites are
`runtime.rs:127` (`submit`), `test_util.rs:60` (`run`), `processes.rs:214`
(test `setup`), and `engine.rs:1412, 1432, 1437, 1487` (tests calling
`cp.parse("…".to_string())`). A pre-wave stub doesn't work: the new call
shapes don't compile against today's `parse`/`submit`, and the old ones don't
compile against C1's. The brief anticipated this for `runtime.rs` ("list it
under Registry entries so the architect applies it"). The other six are the
same kind of edit, in test code. No other wave-1 task touches these files
(F1 owns only `file_parsing/**`).

## Options
1. **C1 makes the edits in one separate, clearly named commit; the architect
   reviews that commit in the merge.** The branch builds and tests green on
   its own, and the diff outside `commands/` is exactly the seven lines the
   plan lists. (Recommended)
2. **The architect applies them in the merge commit.** C1's branch doesn't
   build without them, so the implementer still has to make them locally and
   then drop them, and the planner reviews a branch that doesn't build on its
   own.
3. **Keep the old signatures until E1** (which owns `test_util.rs`,
   `processes`, `engine`). That defers the README §3.5 API by a wave and
   leaves `runtime.rs`'s call for R1. Not recommended: E1 and the wave-3
   family tasks are planned against `&str` and `Parsed`.

## Impact
C1 only. The C1 plan is written for option 1's commit shape (it names the
commit `integration: call-site edits for C1`). If option 2 is chosen, the
architect drops that commit and replays it. If option 3 is chosen, steps 2 and
7 of the plan change.

## Architect review
**Architect, 2026-10-24: option 1.**

C1 makes the call-site edits itself, in one commit
(`integration: call-site edits for C1`) that touches only the lines listed in
`detailed/C1.md` "Registry entries", plus the `Parsed` imports. At merge I
check that `git diff integrate/wave-1...task/C1 -- ':!blast/src/audio_processing/commands'`
contains exactly those edits. Anything more is an ownership violation, and the
branch goes back to the planner.

Reasons:
- The branch builds and tests green on its own, so the planner reviews what
  actually merges.
- Option 2 gives the same end state, but the planner would review a branch
  that doesn't build.
- Option 3 defers the §3.5 API that E1 and the wave-3 plans are built on.

This is a one-off exception to the README §2 ownership rule, for call sites
that a signature change forces. No other wave-1 task owns those files (F1 owns
only `file_parsing/**`), so they can't conflict. The user ratifies it by
approving C1's plan.

Current lines on `integrate/wave-1` @ `2f97859`: `runtime.rs:127`,
`test_util.rs:60`, `processes.rs:214`, `engine.rs:1424, 1444, 1449, 1499`.
C1.md cites the engine lines from before step 0, which are 12 lower.

## Decision
