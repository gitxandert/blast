# REPLb: shell integration, scripts, tab completion
Status: draft   ·   Changelog at the bottom

Part 2 of 2 of REPL (wave 4), after REPLa on the same files. Uses SH's
`shell/` (wave 2; plans `SHa.md`, `SHb.md`, grammar `SH-grammar.md`) and
INS's `completions` (wave 3, README §3.8). Refresh against `integrate/wave-4`
before dispatch, as for REPLa.

## Summary
Every typed line goes through `Shell::feed`, with a continuation prompt
while a block is open. `--script file.blast` and `source <file>` run
scripts whose `wait`s play back on a script thread without blocking typing,
and Ctrl-C stops them. Tab completes command words and names.

## Ownership
- Modifies: `runtime/repl.rs`, `runtime/line_editor.rs` (adds
  `replace_word`).
- Creates: `runtime/script.rs` (**new path; needs the ownership row and a
  registry entry**, see below).
- Reads only: `shell/**`, `commands/**` (`CmdProcessor`, `CmdSender`,
  `Parsed`, `CmdErr`), `runtime/terminal.rs`, `runtime/mod.rs`, `cli.rs`.

## Registry entries
- `runtime/mod.rs`: `mod script;` plus a stub `runtime/script.rs`
  (`//! Script playback thread (REPL).`).
- README §2 wave-4 row for REPL gains `new runtime/script.rs`.
- Depends on REQ-007: a `CmdProcessor` method that submits several lines as
  one `Command::Batch`, all-or-nothing on the mirror. It lives in
  `commands/**` (not REPL's), so another task provides it (see REQ-007).

## Architecture changes
- **Shared submitter (REQ-008).** The brief says `CmdProcessor` lives on the
  REPL thread. This plan moves `CmdProcessor` and `CmdSender` into one
  `Arc<Mutex<Submitter>>` that the REPL thread and script threads both lock
  to submit. Neither lock is reachable from the audio callback (the audio
  side only sees the rtrb consumer), so real-time safety is unchanged. The
  alternative (a channel of lines to the REPL thread) is in REQ-008.
- **One thread per running script.** README §3.7 already puts scripts on
  their own thread; this plan allows several at once (each one is a short-
  lived `std::thread`). No new queue.

## Interfaces
- Provides (crate-internal, `runtime/script.rs`):
  ```rust
  /// Owns the mirror and the command producer; locked by the REPL and script threads.
  pub struct Submitter { pub processor: CmdProcessor, pub sender: CmdSender }
  impl Submitter {
      pub fn line(&mut self, line: &str) -> Result<Option<String>, CmdErr>;     // CmdProcessor::submit
      pub fn batch(&mut self, lines: &[String]) -> Result<Option<String>, CmdErr>; // REQ-007
  }
  /// Where script output goes (tests record it; the REPL prints it).
  pub trait Sink: Send { fn submit(&mut self, out: &ShellOut) -> Result<Option<String>, String>; fn print(&mut self, text: &str); }
  pub struct ScriptHandle { /* name, cancel: Arc<AtomicBool>, thread: JoinHandle<()> */ }
  impl ScriptHandle {
      pub fn spawn(name: String, outs: Vec<ShellOut>, sink: Box<dyn Sink>) -> std::io::Result<Self>;
      pub fn cancel(&self);          // sets the flag and unparks the thread
      pub fn is_finished(&self) -> bool;
  }
  pub fn run_outputs(name: &str, outs: &[ShellOut], sink: &mut dyn Sink, cancel: &AtomicBool); // the runner body; used by tests directly
  ```
- Consumes:
  - SH: `Shell::{new, feed, source, pending, reset, names}`,
    `shell::command_start`, `ShellOut`, `Wait::duration` (REQ-005).
  - INS: `CmdProcessor::completions(&self, line: &str, cursor: usize) -> Vec<String>`
    and `names` (README §3.8). Assumed meaning is in REQ-009.
  - R1: `cli::Options.script: Option<PathBuf>`, `terminal::print_line`
    callable from any thread, the quit path, `CmdSender`.
  - REQ-007's batch submit.
- Contract issues: REQ-007 (atomic batch submit is missing from §3.5),
  REQ-009 (completion semantics unspecified in §3.8).

## Steps
1. **`script.rs` runner.** `run_outputs`: `deadline = Instant::now()`; for
   each output: `Wait(w)` → `deadline += w.duration()`, then sleep until the
   deadline in a loop of `thread::park_timeout(remaining)`, returning early
   when `cancel` is set (a deadline already past just continues: no burst
   catch-up beyond running the lines in order); `Line`/`Batch` →
   `sink.submit`; on `Err(e)` → `sink.print("script <name> stopped at '<line>': <e>")`
   and return; `Ok(Some(text))` → `sink.print(text)`; `Print(t)` →
   `sink.print(t)`. Check `cancel` before every item. `ScriptHandle::spawn`
   uses `thread::Builder::new().name(format!("script {name}"))`.
2. **`Submitter`** and the REPL's `Sink` impl (`ReplSink { submitter:
   Arc<Mutex<Submitter>> }`): lock, call `line`/`batch`, unlock; `print`
   goes to `terminal::print_line`. A poisoned mutex is reported and the
   script stops (no `unwrap`).
3. **Move `CmdProcessor` + `CmdSender` into `Arc<Mutex<Submitter>>`** where
   R1 creates them (`runtime/repl.rs` or `runtime/mod.rs`; if the latter,
   this is a one-line registry edit the architect applies).
4. **Shell integration in the Enter handler.** `history.push(line)`; then
   `shell.feed(line)`:
   - `Err(e)` → `print_line(e)`; prompt back to `Normal`.
   - `Ok(outs)` and `shell.pending()` → prompt `Continuation`.
   - `Ok(outs)` with any `Wait` → `ScriptHandle::spawn("<repl>", outs, …)`.
   - otherwise run `outs` in order on the REPL thread through the same
     `run_outputs` with the REPL sink (no waits, so it doesn't block).
   `Batch` reaches `Submitter::batch` (REQ-007), never `line`.
5. **`source` and `--script`.** `source` needs nothing extra (the shell
   expands it). At startup, if `Options.script` is set:
   `shell.source(path)` and the same dispatch as step 4, named after the
   file; an error is printed and BLAST continues at the prompt.
6. **Ctrl-C** (`Action::Interrupt`), in this order: if any script is
   running, cancel all of them and print `stopped N script(s)`; else if
   searching, cancel the search (REPLa); else if `shell.pending()`,
   `shell.reset()` and clear the line; else if the line isn't empty, clear
   it; else quit as today (R1's quit path). Reap finished handles on every
   loop iteration (`is_finished`, then `join`).
7. **Tab completion.** `LineEditor::replace_word(start_char, text)` replaces
   the chars from `start_char` to the cursor. In `repl.rs`, a
   `Completer { start: usize, candidates: Vec<String>, idx: Option<usize> }`
   alive only while consecutive Tabs are pressed (any other key drops it).
   First Tab: `seg_start = shell::command_start(buf, cursor_byte)`; the
   current word starts after the last whitespace before the cursor. If the
   word starts with `$` → candidates are `shell.names()` variables
   (prefixed `$`); else if it is the first word of the segment → INS
   completions for the segment plus `shell.names()` functions and the
   statement keywords; else → INS `completions(segment, cursor_in_segment)`.
   Sort and dedup. 0 → nothing; 1 → insert it plus a space; several →
   insert their longest common prefix and print them (one `print_line`,
   space-separated, wrapped to the width). Further Tabs cycle through the
   candidates (`Shift-Tab`/`BackTab` cycles back).
8. Update the README module list (`runtime/script.rs`).

## Tests
`script.rs` (a recording `Sink` that timestamps each call):
- `runs_lines_in_order_and_prints`.
- `waits_accumulate_without_drift`: three `Wait(Millis(20))` between four
  lines; each line is ≥ 20·k ms after start (lower bounds only, so the test
  isn't flaky); total < 1 s.
- `cancel_stops_during_wait`: a `Wait(Millis(10_000))`, cancel after 20 ms,
  the thread finishes within 500 ms and later lines never run.
- `error_stops_script_and_reports_line`.
- `batch_goes_to_batch`: a `Batch` reaches the sink as a `Batch`.

`repl.rs`:
- `completer_single_inserts_with_space`, `completer_common_prefix_then_cycle`,
  `completer_other_key_resets` (pure `Completer` with fixed candidate lists).
- `dispatch_without_wait_runs_inline` and `dispatch_with_wait_spawns_script`
  (the dispatch decision as a pure fn over `&[ShellOut]`).
- `interrupt_priority`: the step-6 order, as a pure fn over
  `(scripts_running, searching, pending, line_empty)`.

`line_editor.rs`:
- `replace_word_moves_cursor_to_end_of_insert`.

## Done when
- README §1.4; README module list has `runtime/script.rs`.
- Manual check: example scripts 1–10 from `SH-grammar.md` §9 run from a
  file with `--script` and with `source` (with tracks named like the
  examples generated by
  `ffmpeg -f lavfi -i "sine=frequency=60:duration=0.3" -ar 48000 -ac 2 blast/assets/kick.wav`,
  and the same for `snare`, `hat`, `clap`, `bass`, `pad`); typing works while
  a script waits; Ctrl-C stops it; a `{` block shows the `..` prompt; Tab
  completes `sta` → `start `, `start -v k` → `start -v kick `.

## Risks and open questions
- REQ-007 (batch submit) blocks steps 2 and 4's `Batch` path. Until it
  exists, nothing can submit a `Batch` atomically.
- REQ-008 (shared submitter vs channel) shapes steps 2–3.
- REQ-009 (what `completions` returns) shapes step 7.
- REQ-010: multi-line editing of open blocks is recommended out of scope.
  Today each continuation line is edited on its own and earlier lines of
  the block can't be changed (Ctrl-C drops the block).
- `--script` with no terminal (piped, CI) isn't covered: BLAST still opens
  the prompt afterwards. A `--script … --exit` mode is a follow-up.
- Ctrl-C on a non-empty line now clears it instead of quitting (bash-like).
  This is a user-visible change and part of approving this plan.

## Size
L: three pieces (runner, dispatch/Ctrl-C, completion), each small, with
contracts from three other tasks.

## Changelog
- 2026-10-24: first draft.
