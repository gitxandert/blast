# GRPa: group membership, ungroup, delete
Status: draft   ·   Changelog at the bottom

## Summary
Groups become editable after creation. `group <g> --add a,b` adds top-level
voices to an existing group, `ungroup <g> [a,b]` returns some or all members
to the top level (dissolving the group when none are listed), and
`group --rm <g>` deletes a group together with its members. Removed objects
leave the audio thread through E1's garbage queue, and a group tempo that is
still in use survives as a tempo context instead of breaking references.

Part a of GRP. GRPb (group-wide rate and gain) is separate because it
depends on VOX's `VoiceParam` API and REQ-013.

## Ownership
- **Modifies:** `commands/group.rs` (all of it: `try_group`, `GroupRepr`,
  `GroupArgs`, `HELP`; new `try_ungroup`), `engine/group.rs` (`Group`),
  `engine/handlers/group.rs` (group handlers).
- **Reads only:** `commands/reprs.rs` (`EngineState`, ID allocator),
  `commands/voice.rs` (`VoiceRepr`: GRP sets its `group` field and renames
  its name-index entry, nothing else), `commands/tempo.rs` (`TempoRepr`:
  GRP flips the ownership tag when promoting), `commands/process.rs`
  (`ProcRepr`), `engine/voice.rs` (`Voice.group`, `Voice.state.active`),
  `engine/mod.rs` (arenas, `Limits`, garbage producer), `blast_time.rs`.

## Registry entries
Added by the architect in the wave-3 scaffold, as compiling stubs:
1. `commands/mod.rs` `commands!`: variant `GroupEdit` (args struct
   `GroupEditArgs`, defined as a stub in `commands/group.rs`):
   ```rust
   pub struct GroupEditArgs { pub group: GroupId, pub op: GroupOp }
   #[derive(Clone, Copy, Debug, PartialEq)]
   pub enum GroupOp {
       Join(VoiceId),            // voice becomes a member
       Leave(VoiceId),           // voice returns to the top level
       Dissolve { promote: bool }, // group removed, members already left
       Delete { promote: bool },   // group, members and their processes removed
   }
   ```
2. `commands/mod.rs` parse dispatch: `"ungroup" => self.try_ungroup(args)`
   (stub in `commands/group.rs` returning `CmdErr::NoCmd`). The existing
   `"group"` arm is unchanged (GRPa extends `try_group` itself).
3. Engine `apply` match: `Command::GroupEdit(args) => self.group_edit(args)`
   (stub `fn group_edit(&mut self, _: GroupEditArgs) {}` in
   `engine/handlers/group.rs`).
4. `Garbage::Group(Group)` variant, if E1's `Garbage` doesn't have one.
5. `HELP` rows (in GRP's own `commands/group.rs`; listed for INS):
   - `("group", "group <g> --add <v>[,<v>…]  add top-level voices to a group")`
   - `("group", "group --rm <g>  delete a group, its voices and processes")`
   - `("ungroup", "ungroup <g> [<v>[,<v>…]]  return voices to the top level; no list dissolves the group")`
6. Completion spec rows for `group` and `ungroup` (REQ-017; INS writes them
   if the central-table option is chosen).

## Architecture changes
None. Membership edits run inside the group model of README §3.4 (REQ-011
item 9).

## Interfaces
- **Provides**
  - `fn try_group(&mut self, args: &str) -> CmdResult<Command>`: now three
    forms, chosen by the first tokens: `group <g> [-t …] -v …` (create,
    unchanged), `group <g> --add <list>`, `group --rm <g>`.
  - `fn try_ungroup(&mut self, args: &str) -> CmdResult<Command>`.
  - `fn group_edit(&mut self, args: GroupEditArgs)` on `Conductor`.
  - `pub(crate) fn join_group(&mut self, voice: VoiceId, group: GroupId)`
    on `Conductor` (engine) and its mirror twin
    `fn adopt_into_group(state: &mut EngineState, voice: VoiceId, group: GroupId)`:
    one place for "a member adopts the group's tempo if it has none, and so
    do its voice-following processes". Both group creation (today
    `commands.rs:773-806`, `engine.rs:235-266`) and `--add` use it.
- **Consumes**
  - E1: ID allocator, name index, `Limits::group_members`, arenas,
    `Conductor::remove_voice(VoiceId)`, garbage producer, `Command::Batch`
    (REQ-011 items 1–15).
  - None from TMP or PRC (GRPa writes its own small tempo-user scan, see
    step 6).
- **Contract issues:** none, if REQ-011 holds.

## Behaviour (proposed; decided in REQ-012)
- **`group <g> --add a,b`**: each name must be a top-level voice (a member of
  another group is rejected: `ungroup` it first). The group's member count
  after the add must fit `Limits::group_members`, else `CmdErr::Full`
  (or E1's equivalent) and nothing changes. Each new member is renamed
  `g.a` in the name index; a clash with an existing member name is
  impossible because top-level names are unique and the group's members came
  from the same name space, but check it anyway (a voice loaded later under
  the same name as a member). Tempo adoption follows group creation exactly:
  a voice with no tempo of its own adopts the group's, and so do its
  processes that follow the voice. Playback: the voice keeps its own active
  flag; it's now gated by the group, so joining a paused group silences it.
- **`ungroup <g> [a,b]`**: listed members (or all) return to the top level
  under their bare names. **A name clash with a top-level voice rejects the
  whole command**, naming the clashing voices, and nothing changes. Each
  leaving voice keeps what it hears: `voice.active = voice.active &&
  group.active`. Its tempo reference is unchanged (a TempoId stays valid).
  With no list, after all members leave, the group is dissolved:
  - its own processes are removed (garbage),
  - its own tempo is **promoted to a tempo context named `<g>`** if anything
    still uses it (the ex-members that adopted it, voices loaded with
    `-t g:<g>`, seqs with `-t g:<g>`), otherwise freed. If a context named
    `<g>` exists, the first free of `<g>1`, `<g>2`, … is used (same scheme as
    default seq names, `commands.rs:1392-1400`). `ls tempos` shows it.
  - a group whose tempo was a context reference (`group -t c:clk`) has
    nothing to promote.
- **`group --rm <g>`**: removes the group, every member voice (with their
  processes and own tempos, through E1's voice removal), the group's own
  processes, and the group's tempo, with the same promotion rule for users
  *outside* the group.
- Any parse failure leaves the mirror untouched (the
  `failed_group_leaves_mirror_unchanged` bar).

## Steps
0. **Refresh** against `integrate/wave-3`: map REQ-011's assumed names onto
   E1's actual ones (editorial), and re-read `commands/group.rs`,
   `engine/group.rs`, `engine/handlers/group.rs` as E1 left them. Confirm
   that group creation already goes through one adoption routine; if not,
   step 1 extracts it.
1. **Extract adoption.** In `commands/group.rs`, factor the per-voice part of
   group creation into `adopt_into_group` (mirror) and in
   `engine/handlers/group.rs` into `Conductor::join_group` (engine). Group
   creation calls them. No behaviour change; all existing group tests stay
   green. Rewrite `try_group`'s flag loop with `let … else` while there
   (README §1.3).
2. **Mirror: `--add`.** In `try_group`, if the second token is `--add`,
   parse the comma list (reject empty names, duplicates, unknown or
   already-grouped voices, capacity overflow) *before* touching the mirror.
   Then for each voice: set `VoiceRepr.group`, re-key the name index to
   `g.v`, push to `GroupRepr.members`, call `adopt_into_group`. Return
   `Command::Batch` of `GroupEdit { group, op: Join(v) }` (a single voice
   returns the bare command, not a one-item batch).
3. **Engine: `Join`.** `group_edit` → `GroupOp::Join(v)`: push `v` onto
   `Group.members` (capacity was checked by the mirror; on overflow do
   nothing, since the mirror guarantees it can't happen), set
   `Voice.group = Some(g)`, call `join_group`. No allocation: `members` is
   preallocated.
4. **Mirror: `ungroup`.** `try_ungroup`: parse `<g>` and the optional list,
   validate (members exist; collect top-level name clashes and reject them
   all in one `CmdErr`), then mutate: clear `VoiceRepr.group`, re-key to the
   bare name, remove from `members`. With no list, also dissolve (step 6).
   Returns `Batch[Leave(v)…, (Dissolve { promote })]`.
5. **Engine: `Leave`.** Remove `v` from `Group.members` with `swap_remove`
   on the position (order inside a group has no meaning once voices stay in
   the arena; if E1 kept an order guarantee, use `remove` instead; both are
   allocation-free). Set `Voice.group = None` and
   `voice.state.active &= group.state.active`.
6. **Dissolve / delete, mirror side.** Shared helper
   `fn retire_group(&mut self, g: GroupId, delete_members: bool) -> CmdResult<bool>`
   returns `promote`. It:
   - collects the group's processes and removes their `ProcRepr`s and their
     own tempos from the mirror (freeing IDs);
   - for `delete_members`, removes each member through the same mirror path
     as `unload` (E1), which frees its procs and own tempo;
   - decides promotion by scanning the mirror for users of the group's
     tempo (voices, groups, processes whose tempo reference resolves to that
     `TempoId`) after the removals above. Private helper
     `fn tempo_in_use(state: &EngineState, id: TempoId) -> bool`. TMP has a
     general `tempo_users`; GRPa keeps its own 15-line scan so the two tasks
     can run in parallel (follow-up: dedupe after both merge).
   - promotes (tag `Context`, insert the name `<g>`/`<g>N` into the context
     name index) or frees the tempo ID;
   - removes the `GroupRepr` and its name, and frees the `GroupId`.
   `group --rm <g>` returns `Batch[Delete { promote }]`; members are removed
   by the engine handler (step 7), so the batch stays one command.
7. **Dissolve / delete, engine side.** `GroupOp::Dissolve { promote }` and
   `GroupOp::Delete { promote }`:
   - Delete only: for each member, `self.remove_voice(v)` (E1).
   - Remove the group's processes from the process arena (owner == group)
     into garbage.
   - Tempo: if `promote`, set the tempo's ownership tag to `Context` (it keeps
     ticking with the same phase, active flag unchanged); else remove it from
     the tempo arena.
   - Remove the `Group` from the group arena into `Garbage::Group`.
8. **`HELP` rows** and doc comments. Update the README module list only if
   modules changed (they don't).

## Tests
All mirror+engine tests go through `test_util::setup`/`run` unless noted.
- `group_add_moves_voice_into_group`: `load a,b,c; group g -v a; group g --add b,c`
  → `g.b`, `g.c` resolve; `b` doesn't; engine `Group.members` has 3 IDs and
  each voice's `group == Some(g)`.
- `group_add_adopts_group_tempo`: `load a; load b; seq b -t v -s 0; group g -t s:100 -v a; group g --add b`
  → `b` and its seq use the group's `TempoId` (mirror and engine).
- `group_add_keeps_own_tempo`: `load b -t b:90` then `--add` → `b` keeps its own.
- `group_add_respects_capacity`: Conductor/mirror built with
  `Limits { group_members: 2, .. }`; adding a third member fails and the
  mirror is untouched.
- `failed_group_add_leaves_mirror_unchanged`: table of bad lines
  (`--add nope`, `--add a,a`, `--add h.x` where `x` is in group `h`,
  `--add` with no list, `group nope --add a`) → `assert_untouched`.
- `joined_voice_is_gated_by_group`: playing top-level voice joins a stopped
  group → renders silence; `start -g g` → audible.
- `ungroup_returns_voices_to_top_level`: names re-keyed, engine
  `Voice.group == None`, group gone from mirror and arena.
- `ungroup_partial_list`: `ungroup g a` keeps the group with `b`.
- `ungroup_name_clash_rejected`: `load a; group g -v a; load a` (allowed:
  `load` only checks top-level names, `commands.rs:421-429`), then
  `ungroup g` → error names `a`, mirror and engine untouched.
- `ungroup_keeps_audibility`: members of a paused group are inactive after
  ungroup; members of a playing group keep playing without a gap (render
  across the command: no zero frame).
- `ungroup_removes_group_procs`: `seq -g g …` isn't available until PRCb, so
  build the group seq with a hand-made `Command` the way
  `seq_with_no_steps_or_period_doesnt_panic` does (`processes.rs:285-309`);
  after `ungroup g` the process arena slot is empty and the garbage drain
  saw one process.
- `ungroup_promotes_used_group_tempo`: `group g -t s:100 -v a` (a adopts),
  start, render 250 frames, `ungroup g` → mirror has context `g`; engine
  tempo ID unchanged, still active, `current` continues (render 10 more →
  260).
- `ungroup_promoted_name_avoids_clash`: `tc g b:60` first → promoted name is
  `g1`.
- `ungroup_frees_unused_group_tempo`: `group g -t s:100 -v a` where `a` was
  loaded with `-t b:90` → nothing uses the group tempo → its ID is freed
  (next `tc` reuses it, or the allocator reports it free).
- `group_rm_removes_members_and_procs`: voices gone from mirror and arena,
  garbage saw the voices and the group.
- `group_rm_promotes_tempo_used_outside`: `load d -t g:g` before
  `group --rm g` → `d` still ticks on the promoted context `g`.
- `group_rm_and_ungroup_unknown_group_error`: `CmdErr::NoItem`, mirror
  untouched.
- Existing group tests (C1/E1 ports of `commands.rs:1783-2093` and
  `engine.rs:1141-1398`) stay green after step 1.

## Done when
- The three new forms work end to end through `CmdProcessor::submit` and the
  engine, and every failure path leaves the mirror untouched.
- No allocation or drop of heap data in `group_edit` (trace: `members`
  preallocated; removed objects go to garbage).
- README §1.4.

## Risks and open questions
- Ungroup name clash, process/tempo fate, audibility rule: REQ-012.
- E1 group model and tempo "follow" info: REQ-011 items 4, 9.
- Dispatch order: GRPa is independent of TMP/PRC; merge order in REQ-017.
- Nice-to-have: `group <g> --rename`, moving a voice directly between
  groups (`--add h.x`), and `ungroup --prefix` to resolve clashes
  automatically.
- Two scans of tempo users (GRPa's private one, TMP's `tempo_users`):
  follow-up to dedupe after both merge.

## Size
L: two mirror paths with rollback-free validation, four engine ops, tempo
promotion, ~16 tests. One session if E1's adoption code factors cleanly.

## Changelog
- 2026-10-24: first draft (REQ-011, REQ-012, REQ-017 open).
