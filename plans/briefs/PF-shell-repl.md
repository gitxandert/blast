# PF: Shell language and REPL (SH, REPL)

Read `blast/notes.txt` first. It's the user's own thinking about syntax. BLAST
is meant to be a "Bash-like audio scripting tool". Today it's a
one-line-at-a-time command REPL.

## SH: shell-core (wave 2)

A standalone `shell/**` module (lexer, parser, interpreter) implementing README
§3.7, fully unit-tested, with **no REPL integration** (that's in REPL, wave 4).
Hand-write the lexer and parser. They're small, and a parser crate isn't in
the approved set.

**First deliverable: a grammar proposal for the user** (README §4 item 2):
EBNF plus about ten example scripts. Base it on `notes.txt`:
- `&` command linking, run atomically through `Command::Batch`, and `;`
  sequencing.
- `name { … }` blocks, and the `|` piping idea between processes
  (`seq … | trig`). Pipes need engine work that isn't in this roadmap.
  *Reserve* `|` and say what it will mean. Don't implement it.
- The `all` reserved word. The shell can expand it only if it knows the names,
  so decide whether it asks `CmdProcessor::names` (INS, wave 3) through a
  trait, or whether `all` belongs to the command layer.
- Variables (`let x = …`, `$x`), arithmetic and ranges in arguments, `for`/`if`,
  user functions or aliases, comments, and line continuation.
- `wait <dur>`: tempo-aware units (beats of which tempo? propose one) or plain
  ms. The `Wait` value in `ShellOut` carries it. The runner that actually waits
  belongs to REPL.
- Error messages carry file, line and column.

The shell knows nothing about engine semantics. It only produces plain command
lines.

## REPL: repl (wave 4)

In `runtime/repl.rs`, `runtime/line_editor.rs`, and a new `runtime/history.rs`:
- **Shell integration.** Route every line through `Shell::feed`. Show a
  continuation prompt while a block is open. Batches become `Command::Batch`.
- **Scripts.** `--script file.blast` (R1 reserved the flag) and
  `source <file>`. Run them on a script thread that executes `Wait`s against
  the clock and submits lines. `CmdProcessor` lives on the REPL thread, so
  decide how the script thread reaches it (a channel of lines to the REPL
  thread is the likely answer) and justify the choice. Ctrl-C stops a running
  script without quitting BLAST.
- **Tab completion** via INS's `completions` (README §3.8). One match inserts
  it, several matches list them, repeated Tab cycles through them.
- **Persistent history** in `$XDG_STATE_HOME/blast/history` (falling back to
  `~/.local/state/blast/history`), with a size cap and deduplication of
  consecutive entries. Ctrl-R reverse search.
- **Editing keys.** Ctrl-W, Ctrl-A/E/K/U, and Alt-B/F word motions, using
  crossterm key events (after R1).
- `notes.txt` mentions a bug: "fix typing when cursor is displaced". Reproduce
  it. It may already be fixed.
- Recommend whether multi-line editing of open shell blocks is in scope.
