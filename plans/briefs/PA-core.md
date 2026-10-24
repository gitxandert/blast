# PA: Core restructure (C1, E1, R1)

C1 runs in wave 1, E1 in wave 2, and R1 in wave 3. Each one starts from the
merged result of the one before. Plan them together so the hand-offs line up
exactly. Apart from what README §3 changes, the existing behaviour and the
user-facing command syntax must survive. Port the existing ~180 tests, or
rewrite them where the API changed. Don't drop them.

## C1: commands-split (wave 1)

Split `commands.rs` (about 2400 lines, half of them tests) into the README §3.5
layout, and **rewrite to the new conventions** (README §1.3) as you go:
- `try_*(args: String)` becomes `&str`, and nested `match`/`ok_or` ladders
  become `let … else`/`?`.
- `CmdErr`/`StateErr`: the `cmd_errors!` macro generates two identical enums.
  Decide whether one enum is enough now.
- Introduce `Parsed` and the new `submit` (README §3.5). Keep the current
  `CmdQueue` type; R1 replaces it. `submit` keeps taking `&CmdQueue` until then.
- Give each family file a `HELP` table.
- Move the TODOs at `commands.rs:881-899` and `:1376` unchanged (PRC resolves
  them in wave 3).
- Don't touch `engine.rs` or `runtime.rs`. `use commands::*` must keep
  resolving the same names there, so re-export as needed. `runtime.rs` calls
  `submit(cmd, &queue)`. Update that one call site only if the signature
  forces it, and list it under "Registry entries" so the architect applies it.

## E1: engine-rework (wave 2), the biggest task in the roadmap

Implement README §3.4 (approved by the user). It covers both `engine/**` and
the mirror's ID bookkeeping in `commands/**`.

- **Mirror side.** An ID allocator in `commands/reprs.rs` (one free list per
  kind), and `*Repr`s keyed by ID. Remove index-shift patching (`try_unload`'s
  loop, `TempoRepr::clone_from`'s owner-index logic, `Idx::GroupVoice` if IDs
  make it redundant). `Idx` probably becomes an enum of IDs. Decide.
- **Engine side.** `engine/{mod,render,voice,group,events,arena}.rs`, plus
  `engine/handlers/{voice,group,tempo,process,track}.rs`. `Arena<T>`:
  fixed capacity, `insert(id, T)`, `remove(id) -> Option<T>`,
  `get`/`get_mut`, and iteration over occupied slots. Configured limits go in
  one `Limits` struct.
- **Groups.** Today, voices are *moved into* `Group.voices`. With IDs, a group
  can hold `VoiceId`s while voices stay in the one voice arena, with
  `Voice.group: Option<GroupId>` gating playback. Recommend one model and work
  through what `start`/`stop`/`pause` on a group mean under it. Match today's
  semantics (see the existing group tests).
- **Tempos.** Replace `Rc<RefCell<TempoState>>` with `TempoId` into a tempo
  arena. Evaluate the README's recommendation that the Conductor ticks each
  active tempo once per frame, and check it against every tempo mode
  (`Process`, `Voice`, `Group`, `Context`, `TBD`) and the start/stop/reset
  rules spread across `Voice::start`/`stop` and `Group::start`/`stop`. Whether
  `TempoMode` survives at all, or becomes just an ownership tag, is E1's call.
  Write down the decision.
- **Processes.** `Process` stops holding `Rc`s. `process()` receives what it
  needs: e.g. `&mut VoiceState` plus `&TempoState` looked up by ID. PRC needs a
  stable `ProcId` in wave 3 for enable, disable and remove.
- **Samples.** Tracks become `Arc<[f32]>`. Remove the i16 conversion from
  scaffold step 0. The mix is natively normalised.
- **Queues.** Use `rtrb` for events and garbage (README §3.4). Replace every
  `println!` on the audio thread with an event. Give events a REPL-side
  `fn describe(&Event) -> String`. Nothing drains them until R1, so make sure
  a full event queue just drops events and never blocks.
- **`Command::Batch`.** Also check whether `unsafe impl Send/Sync for Command`
  is still needed. It shouldn't be.
- **`alsa_shim.rs`.** Keeps the current ALSA loop in `runtime.rs` working
  without allocating. Use a scratch buffer preallocated in `prepare`. The
  `coordinate(areas, offset, frames)` signature stays, so `runtime.rs` doesn't
  change.
- **`test_util.rs`.** `render` returns `Vec<f32>` frames. Add a helper that
  runs a command line through the mirror and the engine (`run` already does
  this). Most engine tests should go through it, so they test the mirror and
  the engine together.
- Keep `Function`/`WaveForm` (marked "rewrite later") untouched or delete them.
  Ask the user, since they're the future Track/Wave split from `notes.txt`.

E1 is too big for one session. Split it into sequential parts that each leave
the crate compiling and green, for example: E1a (arenas and IDs, mirror and
engine), E1b (tempo arena and processes), E1c (Arc samples, queues, Batch,
shim). All the parts own the same files and run one after another in wave 2.

## R1: runtime (wave 3)

Implement README §3.6:
- **Split.** `runtime/{mod.rs, audio.rs, repl.rs, terminal.rs}` and
  `runtime/line_editor.rs`.
- **cpal.** Host and device selection, config negotiation, the stream with an
  error callback (surface errors as events), and xrun handling. The callback
  owns the `Conductor` and the rtrb consumers, so it's `Send` after E1.
  `CmdQueue` becomes an rtrb `Producer`/`Consumer` pair behind
  `CmdSender`/`CmdReceiver` (`commands/queue.rs`). Update `submit` to take
  `&mut CmdSender`; this is a registry entry if it lands in `commands/mod.rs`.
- **Startup.** Follow the README §3.3 order (scan, open device, load at the
  accepted config), using LOAD's API. Print the chosen device and config.
- **crossterm.** Raw mode, key events mapped onto `LineEditor`, and resize
  events. All output goes through one `terminal::print_line` that handles
  raw-mode line endings and redraws the prompt afterwards. Drain the event
  queue (`Event` → `describe`) and the garbage queue (drop) on the REPL side.
  The redraw thread is the likely place; decide.
- **lexopt.** `cli.rs` with `--device`, `--list-devices`, `--script` (reserved
  for REPL in wave 4), and the positional assets dir.
- **Cleanup.** Delete `engine/alsa_shim.rs`, the hand-written ALSA code, termios
  and `parse_key` (keep `parse_key`'s tests only if they still apply), and
  `libc::raise`. Note the follow-up: the architect removes `alsa-sys`/`libc`
  and adds `#![forbid(unsafe_code)]`. Confirm nothing else needs `unsafe`.
- **Latency.** Today's ALSA config is a 128-frame period with a buffer of
  4 periods. Match it with cpal where the device allows, and report the
  latency actually achieved.
