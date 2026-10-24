---
id: REQ-008
type: architecture
from: planner (PF-shell-repl, REPL)
tasks: [REPLb, R1]
blocks: [REPLb]
status: open
created: 2026-10-24
---

## Request
Decide how a script thread reaches `CmdProcessor`: share it (with the
`CmdSender`) behind an `Arc<Mutex<Submitter>>`, instead of sending lines
over a channel to the REPL thread.

## Context
The brief: "`CmdProcessor` lives on the REPL thread, so decide how the script
thread reaches it (a channel of lines to the REPL thread is the likely
answer) and justify the choice." Plan: `detailed/REPLb.md` steps 1–3. Scripts
are expanded up front by the shell (on the REPL thread), so the script thread
only needs to sleep through `Wait`s and submit lines at the right time.

## Options
1. **`Arc<Mutex<Submitter { processor, sender }>>`**, locked by the REPL
   thread (typing) and by each script thread (on each line). (Recommended.)
   - Timing: a script line is submitted the moment its deadline passes;
     the only added latency is the uncontended lock (microseconds).
   - The REPL thread can keep blocking in `crossterm::event::read()`; no
     polling loop.
   - Real-time safety unchanged: the audio thread never sees the mutex
     (it only holds the rtrb consumer).
   - Cost: `CmdProcessor` must be `Send` (it is after E1 removes `Rc`), and
     error printing happens from the script thread through
     `terminal::print_line` (which R1 already has to make thread-safe for the
     redraw thread).
2. **Channel of lines to the REPL thread** (`std::sync::mpsc`).
   - The REPL thread must wake for both keys and script lines, so it must
     poll: `event::poll(timeout)` then drain the channel. Each script line is
     late by up to the poll timeout (e.g. 1–5 ms of jitter, on top of the
     audio period), and idle wake-ups cost CPU.
   - Keeps `CmdProcessor` strictly single-threaded.

## Impact
Option 1 touches where R1 creates `CmdProcessor`/`CmdSender` (one place,
moved into the `Submitter` by REPLb; a one-line registry edit if it's in
`runtime/mod.rs`). Option 2 needs a polling REPL loop and accepts timing
jitter. Neither touches the engine or any queue to the audio thread.

## Architect review

## Decision
