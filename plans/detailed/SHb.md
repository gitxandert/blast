# SHb: shell interpreter and public API
Status: draft   ·   Changelog at the bottom

Part 2 of 2 of SH (shell-core, wave 2). Runs after SHa in the same wave, on
the same files. Language:
`/home/alexander-toth/rust_proj/blast/plans/detailed/SH-grammar.md` (REQ-004).

## Summary
`Shell::feed` works: it accumulates lines until a statement is complete,
then runs it (variables, functions, loops, `if`, `source`, batches, named
blocks, `wait`, `print`) and returns `ShellOut`s. Everything is unit-tested
with no REPL integration (REPL does that in wave 4).

## Ownership
- Creates: `shell/interp.rs`.
- Modifies: `shell/mod.rs` (public API), any `shell/**` file from SHa.
- Reads only: nothing outside `shell/`.

## Registry entries
None beyond SHa's.

## Architecture changes
README §3.7 gives `Shell::feed` and `ShellOut`. This plan adds, without
changing those: `Shell::new`, `Shell::with_loader`, `Shell::source`,
`Shell::pending`, `Shell::reset`, `Shell::names`, `shell::command_start`, the
concrete `Wait` and `ShellErr` types, and the rule that `source` is expanded
inside the shell (through a loader) rather than by the REPL. All additive.
Filed as **REQ-005** so the architect can record them in §3.7.

## Interfaces
- Provides (`pub`, re-exported from `shell/mod.rs`):
  ```rust
  /// A text-to-command-lines interpreter. Holds variables, functions and any open block.
  pub struct Shell { /* globals, fns, pending: String, pending_line: u32, loader, src_stack */ }

  pub type Loader = Box<dyn FnMut(&std::path::Path) -> std::io::Result<String> + Send>;

  impl Shell {
      /// A shell whose `source` reads files with `std::fs::read_to_string`.
      pub fn new() -> Self;
      /// A shell with a custom file loader (tests use an in-memory map).
      pub fn with_loader(loader: Loader) -> Self;
      /// Feed one input line. Returns finished output, or nothing while a block is still open.
      pub fn feed(&mut self, line: &str) -> Result<Vec<ShellOut>, ShellErr>;
      /// Run a whole script file in this shell (what `source <path>` does). Used by `--script`.
      pub fn source(&mut self, path: &str) -> Result<Vec<ShellOut>, ShellErr>;
      /// True while `feed` is holding an unfinished statement (open `{`, trailing `&` or `\`).
      pub fn pending(&self) -> bool;
      /// Drop an unfinished statement (Ctrl-C at a continuation prompt).
      pub fn reset(&mut self);
      /// Names of defined functions and global variables, sorted (for completion).
      pub fn names(&self) -> Vec<String>;
  }
  impl Default for Shell { fn default() -> Self { Self::new() } }

  /// Byte offset where the simple command containing byte `cursor` starts
  /// (after the last unquoted `;`, `&`, `{`, `}` or newline before it). For completion.
  pub fn command_start(line: &str, cursor: usize) -> usize;

  #[derive(Debug, Clone, PartialEq)]
  pub enum ShellOut { Line(String), Batch(Vec<String>), Wait(Wait), Print(String) }

  #[derive(Debug, Clone, Copy, PartialEq)]
  pub enum Wait { Millis(f64), Beats { beats: f64, bpm: f64 } }
  impl Wait { /// Wall-clock length. pub fn duration(&self) -> std::time::Duration; }
  ```
- Consumes: SHa's crate-internal API.
- Contract issues: `ShellOut` and `feed` are exactly README §3.7. Everything
  else is additive (REQ-005). `all` is passed through (REQ-006).

## Steps
1. **State.** In `interp.rs`: `struct Frame { vars: HashMap<String, Value> }`;
   `Shell` holds `globals: Frame` (with `bpm = 120` predefined),
   `fns: HashMap<String, FnDef { params: Vec<String>, body: Arc<[Stmt]> }>`
   (`Arc`, so a call doesn't clone the body and `Shell` stays `Send`; `FnDef`
   also keeps `src_name: Arc<str>` and `src_text: Arc<str>` of the text it was
   defined in, so errors inside a body point at the definition's file/line),
   `pending: String`, the
   loader, and `src_stack: Vec<SrcCtx { name: String, dir: PathBuf }>`.
   `Shell` must be `Send` (REPL may move it); add a compile-time assert.
2. **Run context.** `struct Run<'a> { shell: &'a mut Shell, out: Vec<ShellOut>,
   locals: Option<Frame>, depth: u32, batch: Option<Vec<String>>, self_name:
   Option<String>, src: &'a str (text, for line/col) }`. Variable lookup:
   locals (and `self` inside a named block), then globals. `let` writes to
   locals when inside a call, else globals.
3. **Word expansion.** `expand(&Word) -> Result<String, ShellErr>`: concat
   parts; `Var` looks up and `to_word`s; `Arith` evals and `to_word`s.
   For `for` items, a word that is exactly one `Var` whose value is a list
   splices the list. For command lines, a word that was quoted and contains
   whitespace after expansion is `WhitespaceInWord`. `all`, `|`-in-words
   etc. pass through untouched.
4. **Statements.**
   - `Linked(simples)`: for each simple, if its first word (unquoted,
     literal) names a function, call it (step 5) with the expanded remaining
     words as args; else expand all words and join with one space into a
     command line. One resulting line → `Line`, several → `Batch`. While
     inside a `Linked` with more than one element or inside a named block,
     command lines go into `run.batch` instead of `out`, and `Wait` raises
     `WaitInBatch` (this catches waits reached through function calls).
     `Print` inside a batch is emitted before the batch (documented).
   - `Named{name, body}`: set `self_name`, open a batch, run body, close
     batch (emit `Line`/`Batch`, or nothing if empty).
   - `Let`, `If`, `For` (range: eval to list; words: expand/splice; bind the
     loop var in the current frame each iteration), `Fn` (store; redefining
     replaces), `Print`, `Wait` (parse `N`, `Nms`, `Ns`, `Nb` with N a finite
     number ≥ 0; `b` uses the current `bpm` value, which must be a number
     > 0; a `@` is `BadWait("tempo-context waits are reserved")`).
   - `Source{word}`: resolve the path against `src_stack.last().dir` (or the
     current dir at the prompt), check depth ≤ 16 (`SourceDepth`), call the
     loader (`SourceIo` on failure), parse the whole text (`Incomplete` →
     `UnexpectedEof` at the end of that file), push `SrcCtx`, run, pop.
     Errors inside carry that file's name and line.
5. **Calls.** Check arity (`Arity`), depth ≤ 64 (`RecursionLimit`), bind
   params in a fresh `Frame`, run the body with that frame, restore.
6. **Output cap.** Count every emitted item and every line placed in a
   batch; past 100 000 return `OutputLimit`.
7. **`feed`.** Append `line` and `'\n'` to `pending`. `parse(&pending)`:
   `Incomplete` → `Ok(vec![])`; `At(span, kind)` → clear `pending`, return
   `ShellErr { src: "<repl>", line/col from span }`; `Ok(stmts)` → clear,
   run, return outputs. On a run error nothing is returned (all-or-nothing
   per statement group; partial output is discarded). State changes made
   before the error (e.g. a `let`) stay, like Bash.
8. **`source` (pub)**, `pending`, `reset`, `names`, `command_start` (reuse
   the lexer's quote/escape tracking; a small scanner is fine), `Wait::duration`
   (`Beats`: `beats * 60.0 / bpm` seconds; negative or non-finite clamp to 0).
9. Update the README module list with `shell/` (one line).

## Tests
In `interp.rs` (or `shell/tests.rs`), all through the public API:
- `examples_produce_documented_output`: each of the 10 example scripts in
  `SH-grammar.md` §9, fed line by line, gives exactly the output listed
  there (example 10 uses an in-memory loader holding `lib/drums.blast`).
- `rejected_examples_error_with_location`: each rejected example gives its
  `ShellErrKind` and `src:line:col`.
- `feed_waits_for_open_block`: `kick {` → `[]` and `pending()`; `}` completes it.
- `reset_discards_pending`.
- `single_link_is_line_not_batch`: `a & b` is `Batch`, `a` is `Line`, `x {}` is nothing.
- `wait_units`: `250`, `250ms`, `1.5s`, `2b` with bpm 90; `Wait::duration` values; `4b@main` and `-1` error.
- `wait_in_batch_via_function_errors`: `fn w { wait 1 }` then `x { w }`.
- `function_locals_dont_leak` and `let_at_top_level_is_global`.
- `for_splices_list_variable`: `let s = 0..3` then `for i in $s {print $i}`.
- `source_relative_path_and_error_location`: a sourced file's error reports that file's name and line.
- `source_cycle_hits_depth_limit`.
- `recursion_limit` and `output_limit` (a nested range loop).
- `all_passes_through`: `stop -v all` → `Line("stop -v all")`.
- `names_lists_fns_and_globals`.
- `command_start_finds_segment`: for `a & seq k -s` with the cursor at the end, the start is after `& `; quoted `;` doesn't count.
- `shell_is_send` (compile-time).

## Done when
- README §1.4, plus: the documented outputs of all 10 examples are tests; no
  `unwrap`/`expect` on input; clippy clean in `shell/**`; README module list
  has `shell/`.

## Risks and open questions
- REQ-004 (grammar), REQ-005 (API additions), REQ-006 (`all` in the command
  layer) can each change details. Steps 3–8 depend on REQ-004; the `pub`
  signatures depend on REQ-005.
- Errors from the *command layer* (e.g. unknown voice) can't point at a
  script line, because `ShellOut::Line` carries no location. REPL shows the
  failing line's text instead. A follow-up could add a location to
  `ShellOut::Line`, which would be a contract change.
- Partial output on error is discarded. Alternative: return what was
  produced before the error. Discarding is safer (no half-run scripts).

## Size
M: about 700 lines including tests; the hard parts (lexing, parsing) are
done in SHa.

## Changelog
- 2026-10-24: first draft.
