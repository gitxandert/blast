---
id: REQ-004
type: question
from: planner (PF-shell-repl, SH)
tasks: [SH, SHa, SHb, REPL]
blocks: [SHa, SHb]
status: awaiting-user
created: 2026-10-24
---

## Request
Approve the concrete shell grammar in
`/home/alexander-toth/rust_proj/blast/plans/detailed/SH-grammar.md`
(decisions.md item 2 requires user approval before SH is implemented).

## Context
README §4 item 2 / decisions.md 2: start from `notes.txt` plus `let`, `for`,
`if`, `fn`, `source`; the SH planner proposes a grammar. The file has EBNF,
semantics, and ten example scripts with their exact output. Seven decision
points are marked **[D-1]..[D-7]**; each can be decided on its own. The
user approves one item at a time, so the PM can present them one by one.

## Options
Approve the grammar as written, with these recommended answers:
1. **[D-1] `|` is an operator only when it stands alone.** The command layer
   already uses `|` inside words (`seq -j 0:0.1|0.2`), so this keeps today's
   commands working unchanged. (Recommended.) Alternative: always an
   operator, and change `seq -j` to another separator (PRC/EUC work).
2. **[D-2] `name { … }` is a named atomic block**: its command lines become
   one `Batch`, with `$self` = `name`; it's where pipelines go later.
   (Recommended.) Alternative: reserve `name { … }` entirely (error today)
   until pipes exist.
3. **[D-3] `a & b` links commands into one atomic `Batch`**; a trailing `&`
   continues onto the next line. (Recommended; from notes.txt.)
4. **[D-4] Pipes are reserved**: `a | b` is an error today, with the stated
   future meaning (process `a`'s events drive process `b`). (Recommended; the
   brief says not to implement them.)
5. **[D-5] Lists expand comma-joined** (`-s $steps` → `-s 0,4,8,12`), matching
   the command layer's list convention. (Recommended.) Alternative:
   space-joined, which today's commands don't accept for steps or voices.
6. **[D-6] `wait` units**: bare/`ms`, `s`, and `b` = beats of the shell's
   own `bpm` variable (default 120); `wait 4b@<tempo>` reserved for later.
   (Recommended.) Alternative: milliseconds only.
7. **[D-7] `all` belongs to the command layer**, passed through by the
   shell (details in REQ-006). (Recommended.)

Also part of the proposal (no alternatives offered, but the user may object):
undefined variables are errors (unlike Bash); no `while`/`loop`, so every
script is finite and expands up front; functions are `fn name a b { … }`
and may shadow commands; no aliases.

## Impact
SHa/SHb implement exactly the approved file. D-1 alternative would add work
to PRC/EUC (seq's jitter syntax). D-2 alternative removes one feature from
SHb. D-6 alternative simplifies `Wait` to milliseconds. No other task
depends on the grammar except REPL (which only consumes `ShellOut`).

## Architect review
**Architect, 2026-10-24: conforms. Recommend approving it, with the
recommended answers to D-1..D-7 and two fixes to the grammar text.**

On the architecture side, the grammar stays inside §3.7. It only produces
`Line`/`Batch`/`Wait`/`Print` and knows no engine semantics. It starts from
`notes.txt` plus `let/for/if/fn/source` (§4.2). D-1 keeps every existing
command's syntax working. D-7 is right (REQ-006).

Fixes, which the planner makes before the user sees the final version:
1. **Bound the work, not just the output.** "Every script stays finite"
   isn't enough. `for i in 0..65536 { for j in 0..65536 { let x = 1 } }` emits
   nothing and runs about 4·10⁹ steps on the REPL thread. Add a
   statement/iteration budget per `feed`/`source` (e.g. 1 000 000 steps →
   `StepLimit` error), next to the 100 000-output cap.
2. **Interactive `else` on the next line.** At the prompt, `feed` runs
   `if … { … }` as soon as `}` closes it, so an `else` typed on the following
   line arrives as a stray statement. Specify the rule: at the top level of
   interactive input, `else` must be on the same line as `}`. Inside an open
   block and in files, the whole text is parsed at once, so the next line
   is fine.

Editorial: add the expression keywords `and`, `or`, `not` to the reserved
words, so `let and = 1` is rejected.

D-3 (`&` as an atomic `Batch`) depends on an atomic batch submit in the
command layer (REQ-007, not reviewed yet). The grammar itself is unaffected.

## Decision
