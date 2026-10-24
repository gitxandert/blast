# PH: Engine commands (GRP, TMP, PRC, INS), all wave 3

These tasks build on README §3.4–3.5 (after C1 and E1): stable IDs, slot arenas, and the garbage queue. Each one adds
commands in its own family file and a handler in `engine/handlers/<family>.rs`.
The architect registers the Command variants and the parse/apply arms in the
scaffold step. Each plan lists them exactly under "Registry entries". Read
`blast/notes.txt` for the user's intended syntax direction (`-n/--name`,
generic verbs like `add`/`assign`, the `all` keyword). Stay consistent with the
flag style of the existing commands.

The command mirror (`EngineState` / `*Repr`) and the engine must agree at all
times. The existing tests `failed_group_leaves_mirror_unchanged` and
`full_queue_leaves_mirror_unchanged` set the bar: any command that fails to
parse or to queue leaves the mirror untouched.

## GRP: group-management

- `ungroup <g>`: Voices return to the top level with their names (decide what
  happens on a name clash). Group-owned Processes and the Group's own
  tempo: decide what happens to them. Voices whose tempo pointed at the Group
  need a new tempo. Decide which.
- `group <g> --add <v>…` and `group <g> --remove <v>…` (or the `add`/`fuse`
  verbs from notes.txt; propose one). With stable IDs this is membership
  bookkeeping only, with no index shifting. Respect the group's preallocated
  member capacity.
- Unload or delete a whole group.
- `velocity -g <g> <val>`: probably scales every member, or becomes a
  group-level rate. Decide which. (VOX owns voice rate semantics, so coordinate
  through the README §3.9 `VoiceParam` contract rather than editing
  `voice.rs`.)
- Group gain (`GroupState.gain` exists but nothing sets it).
- Deleted objects go to E1's garbage queue.

## TMP: tempo-management

- Change a tempo context's interval and unit (`tc <name> -t bpm:140`).
  `TempoState::set_interval` exists and is unused. Do the same for a voice's
  own tempo, a group's tempo, and a process tempo. After E1, every tempo is a
  `TempoId` in the tempo arena.
- Phase behaviour when the interval changes mid-play: keep the phase position,
  don't jump. Specify this precisely in terms of `TempoState`'s fields after E1,
  and check that `Seq::fire`'s wrap/crossing logic stays correct.
- Remove a tempo context (`tc --rm`), rejected while something still references
  it. The mirror has to know what references it; plan that bookkeeping.
- Reassign an object's tempo (`assign <obj> <tc>`, as in notes.txt).
- Optional, if small: tempo ramps (accelerando). Otherwise list it as a
  nice-to-have.

## PRC: process-management

- `seq` on a Group: resolve the TODOs in `commands/procs/seq.rs` (originally
  `commands.rs:881-899` and `:1376`). The engine already runs group-owned
  Seqs (`process_group`). Only the parsing and the mirror are missing. Decide
  the syntax that targets a group as opposed to a voice.
- Address processes by name (`voice.proc`, `group.proc`) through a generic
  lookup, replacing the hand-rolled one in `try_seq`. `ProcRepr.owner_idx` is
  currently dead (`#[allow(dead_code)]`).
- Enable, disable and remove a process by `ProcId` (from E1).
  `SeqState.active` has a TODO for this. Removal drops through the garbage
  queue.
- Edit an existing seq in place (steps, chance, jitter, period) without
  recreating it. Recommend whether this is in scope.
- User-defined seed (`--seed <u64>`), the TODO at `processes.rs:72`.
  `blast_rand.rs` already has `X128P::new(seed)`.

## INS: inspection-help

- `ls` / `list [tracks|voices|groups|tempos|procs]`, read from the mirror and
  returned as `Parsed::Local`.
- `info <name>`: details of one object (tempo, processes, group membership).
- `help` and `help <cmd>`, built from every family's `HELP` table. The help
  index is a static list of the `HELP` tables; that list is a registry entry.
- `completions` and `names` (README §3.8). REPL consumes them in wave 4.
  Completion has to know each command's argument grammar to suggest the right
  kind of name. Propose a lightweight, declarative per-command arg spec,
  without rewriting the parsers.
- Live playing state (which voices are sounding) isn't in the mirror. Decide
  whether `status` shows it (via E1's event queue, e.g. `Event::Ended`) or
  leaves it out. New event kinds are registry entries.
