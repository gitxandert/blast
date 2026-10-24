# REPLa: line editing, persistent history, reverse search
Status: draft   ·   Changelog at the bottom

Part 1 of 2 of REPL (wave 4). REPLb (shell, scripts, completion) follows in
the same wave on the same files. Both build on R1's wave-3 runtime: before
dispatch the planner re-reads `runtime/**` on `integrate/wave-4` and refreshes
the line references and the "Consumes" list below (README §2, `roles/planner.md`
job 2). Today's code is cited at its pre-R1 location
(`audio_processing/line_editor.rs`, `audio_processing/runtime.rs`).

## Summary
The prompt gets readline-style editing keys (Ctrl-W/A/E/K/U, Alt-B/F),
history that survives restarts (`$XDG_STATE_HOME/blast/history`, capped,
consecutive duplicates dropped) with Ctrl-R reverse search, and a line that
never wraps, which fixes the "typing when cursor is displaced" bug from
`notes.txt` for long lines.

## Ownership
- Modifies: `runtime/line_editor.rs` (all of it), `runtime/repl.rs` (the
  key-handling loop and its history use).
- Creates: `runtime/history.rs`.
- Reads only: `runtime/terminal.rs`, `runtime/mod.rs` (R1), crossterm 0.29.0.

## Registry entries
- `runtime/mod.rs`: `mod history;` (with an empty `runtime/history.rs` stub
  `//! Persistent REPL history (REPL).`).

## Architecture changes
None. History is a file the REPL thread reads at startup and appends to on
Enter; no new thread.

## Interfaces
- Provides (crate-internal):
  ```rust
  // line_editor.rs (additions; existing methods keep their behaviour)
  pub enum Prompt { Normal, Continuation, Search { query: String, failed: bool } }
  impl LineEditor {
      pub fn set_prompt(&mut self, p: Prompt);
      pub fn set_width(&mut self, cols: u16);      // from crossterm::terminal::size() and Resize events
      pub fn word_left(&mut self);                 // Alt-B
      pub fn word_right(&mut self);                // Alt-F
      pub fn kill_word_back(&mut self);            // Ctrl-W
      pub fn kill_to_end(&mut self);               // Ctrl-K
      pub fn kill_to_start(&mut self);             // Ctrl-U
      pub fn render(&self, marker: char) -> String; // same signature; now prompt- and width-aware
  }
  // history.rs
  pub struct History { /* entries, cap, path, nav */ }
  impl History {
      pub fn open(path: Option<PathBuf>, cap: usize) -> (Self, Option<String>); // String = warning to print
      pub fn push(&mut self, line: &str) -> Option<String>;                    // warning on write failure
      pub fn prev(&mut self, current: &str) -> Option<&str>;                   // Up
      pub fn next(&mut self) -> Option<&str>;                                  // Down; None past newest = restore saved
      pub fn search(&self, query: &str, before: usize) -> Option<usize>;       // newest index < before containing query
      pub fn get(&self, i: usize) -> Option<&str>;
  }
  pub fn history_path(xdg_state: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf>;
  pub const HISTORY_CAP: usize = 1000;
  ```
- Consumes (from R1, to be confirmed on `integrate/wave-4`): the key loop in
  `runtime/repl.rs` reading `crossterm::event::read()` and calling
  `LineEditor` methods; the shared `Arc<Mutex<LineEditor>>` that the redraw
  thread renders with `render(marker)`; `terminal::print_line`.
- Contract issues: none. Keeping `render(&self, marker)` unchanged (the
  editor stores prompt and width) means the redraw code R1 owns needs no edit.

## Steps
1. **Reproduce the `notes.txt` cursor bug.** On `dev` the single-row case is
   already right (`render` at `line_editor.rs:102-109`, tests at `:415-439`).
   Check at the prompt on `integrate/wave-4`: (a) type past the terminal
   width, then Left/Backspace; (b) a non-ASCII char; (c) an error message
   while typing. Expected: (a) is still broken (`\r` only returns to the
   start of the *last* wrapped row), (b) and (c) fine after R1's
   `print_line`. Record the findings in the report.
2. **Width-aware render.** `LineEditor` gets `prompt: Prompt` and
   `width: u16` (default 80). `render`: prompt text is `"{marker} "` for
   `Normal`, `".. "` for `Continuation` (the marker keeps animating in front:
   `"{marker}.. "`), and `(search)'{query}': ` (or `(failed search)…`) for
   `Search`. If prompt + line fits in `width - 1` columns, output as today.
   Otherwise show a window of the buffer that contains the cursor: start at
   `max(0, cur - visible + 1)` chars, so the cursor is always visible and the
   output never reaches the last column (no wrap). One column per char, as
   today (documented limitation for wide chars).
3. **Editing keys** in `LineEditor`, readline semantics: a "word" for
   Alt-B/F is a run of alphanumerics; Ctrl-W deletes back to the previous
   whitespace (unix-word-rubout, so `-t c:main` loses `c:main` at once).
   Ctrl-K/U delete to end/start. No kill ring (Ctrl-Y is a follow-up).
4. **`history.rs`.** `history_path`: `$XDG_STATE_HOME/blast/history` when
   set, non-empty and absolute, else `$HOME/.local/state/blast/history`, else
   `None` (history stays in memory). `open`: create the directory, read
   lines (a missing file is fine; a read error is a warning and in-memory
   history), keep the last `cap`, and rewrite the file if it held more than
   `2 * cap` lines. `push`: ignore empty lines and lines equal to the newest
   entry; append to memory (drop the oldest past `cap`) and append one line
   to the file (open with `append(true).create(true)`). Navigation (`prev`/
   `next`) saves the line being typed and restores it past the newest entry
   (today's behaviour at `runtime.rs:157-171` drops it).
5. **Reverse search** in `repl.rs`: a small state `SearchState { query,
   hit: Option<usize>, saved: String }`. Ctrl-R enters it (or, inside it,
   finds the next older match); typed chars extend `query` and search from
   the newest entry; Backspace shortens it; the editor shows the hit, prompt
   `Search{..}`. Enter accepts and runs the line; Esc, Ctrl-G and Ctrl-C
   cancel and restore `saved`; any other editing key accepts the hit into the
   editor and is then applied.
6. **Key mapping.** Replace the R1 key dispatch with a pure function
   `fn key_action(ev: &KeyEvent, searching: bool) -> Action` (an enum of
   editor ops, `Submit`, `Interrupt`, `HistoryPrev/Next`, `SearchStart`,
   `Tab` (no-op until REPLb), `Ignore`). Ignore `KeyEventKind::Release`.
   Map Ctrl-A/E, Ctrl-W/K/U, Alt-B/F, Ctrl-R, Ctrl-G, Esc, plus everything
   R1 already maps. `Event::Resize(cols, _)` calls `set_width`; at startup use
   `crossterm::terminal::size()`.
7. Startup: `History::open(history_path(env::var_os("XDG_STATE_HOME").as_deref(), env::var_os("HOME").as_deref()), HISTORY_CAP)`;
   print the warning, if any, through `print_line`.

## Tests
`line_editor.rs` (keep all existing editor/render tests; `parse_key` tests
go with R1):
- `word_motions`: Alt-B/F over `seq kick -s 0,4` land on word starts/ends.
- `kill_word_back_is_whitespace_delimited`: `load kick -t c:main|` → `load kick -t |`.
- `kill_to_end_and_start`.
- `render_long_line_never_reaches_width`: width 20, a 50-char line, the
  cursor at the end, middle and start; the visible text plus prompt is < 20
  columns and the cursor move-back is right.
- `render_continuation_and_search_prompts`.
- `render_narrow_terminal_does_not_panic`: width 0..4.

`history.rs` (temp files under `std::env::temp_dir()` with a unique name,
removed at the end):
- `history_path_prefers_xdg_then_home`: relative XDG is ignored; neither set → `None`.
- `push_skips_empty_and_consecutive_duplicates`.
- `cap_drops_oldest_and_compacts_file`.
- `persist_and_reload_roundtrip`.
- `missing_dir_is_created`, `unwritable_path_warns_not_panics` (a path under a regular file).
- `prev_next_restores_typed_line`.
- `search_finds_newest_then_older`.

`repl.rs`:
- `key_action_table`: every mapped chord gives the right `Action`; key
  release events are ignored.

## Done when
- README §1.4; the cursor-bug findings are in the report; the README module
  list has `runtime/history.rs`.
- Manual check at the prompt: long lines scroll, every listed key works,
  history survives a restart, Ctrl-R finds entries.

## Risks and open questions
- R1 hasn't been planned in detail yet (planner PA, concurrently). If R1's
  redraw code calls something other than `render(marker)`, or puts the key
  loop outside `runtime/repl.rs`, this plan needs a refresh (editorial if
  only names move; a `plan-change` if ownership moves).
- crossterm is built with only `events` (README §1.1), so bracketed paste is
  off: pasting a multi-line script arrives as keys and Enters, which the
  shell handles line by line (REPLb). Fine.
- Wide (CJK, emoji) characters still count as one column. A width table
  needs a crate (`unicode-width`), so it's a follow-up, not proposed.

## Size
M: editor ops and history are small and pure; the key table is mechanical.

## Changelog
- 2026-10-24: first draft.
