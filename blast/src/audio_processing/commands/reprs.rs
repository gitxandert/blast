//! The command thread's mirror of engine state, and the ID authority
//! (README §3.4): every ID is allocated here, from a free list, before the
//! Command that uses it is queued.

use super::buffer::{BufSaves, BufferRepr};
use super::errors::{CmdErr, CmdResult};
use super::group::GroupRepr;
use super::lang::Code;
use super::process::ProcRepr;
use super::procs::packet::PacketRepr;
use super::tempo::TempoRepr;
use super::track::TrackRepr;
use super::voice::{UnloadArgs, VoiceRepr};
use super::{
    AnyId, BatchArgs, Command, FxOwner, GlideRef, GroupOp, WhenSpec, created_ids, effect_ids,
    fm_modulator_ids, visit_ids,
};
use crate::audio_processing::engine::BufSource;
use crate::audio_processing::engine::WaveParam;
use crate::audio_processing::engine::arena::Limits;
use crate::audio_processing::engine::fx::own_settings;
use crate::file_parsing::decode_helpers::AudioFile;
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

macro_rules! ids {
    ( $( $(#[$doc:meta])* $name:ident ),* $(,)? ) => {
        $(
            $(#[$doc])*
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
            pub struct $name(pub u32);

            impl $name {
                /// The arena slot this ID names.
                pub fn index(self) -> usize {
                    self.0 as usize
                }
            }

            impl From<u32> for $name {
                fn from(n: u32) -> Self {
                    Self(n)
                }
            }

            impl From<$name> for u32 {
                fn from(id: $name) -> u32 {
                    id.0
                }
            }
        )*
    };
}

ids! {
    /// A Voice's stable ID.
    VoiceId,
    /// A Group's stable ID.
    GroupId,
    /// A TempoState's stable ID.
    TempoId,
    /// A Process's stable ID.
    ProcId,
    /// A track's ID: its position in the name-sorted track list.
    TrackId,
    /// A buffer's slot in the engine's table.
    BufferId,
    /// A packet's ID. Packets exist only in the mirror (their seqs are the
    /// engine's), so IDs are counted up and never reused.
    PacketId,
}

/// Free-list allocator for one ID kind. `alloc` reuses the most recently
/// freed ID first, and returns `None` once `limit` IDs are live.
#[derive(Clone)]
pub struct IdPool<I> {
    free: Vec<u32>,
    /// Every ID freed since `take_freed`, even if it was handed out again
    /// (the waiting lines that named it are stale either way).
    freed: Vec<u32>,
    /// Every ID handed out since `take_allocated` (the journal a waiting
    /// statement's record is made from).
    allocated: Vec<u32>,
    /// IDs of objects a waiting line will remove: not live for the mirror,
    /// not free either, since the engine still has the object (PKb2).
    held: Vec<u32>,
    next: u32,
    limit: u32,
    _kind: PhantomData<I>,
}

impl<I: From<u32> + Into<u32> + Copy> IdPool<I> {
    /// An empty pool handing out at most `limit` live IDs.
    pub fn new(limit: u32) -> Self {
        Self {
            free: Vec::new(),
            freed: Vec::new(),
            allocated: Vec::new(),
            held: Vec::new(),
            next: 0,
            limit,
            _kind: PhantomData,
        }
    }

    /// A free ID, or `None` if `limit` IDs are live.
    pub fn alloc(&mut self) -> Option<I> {
        let n = match self.free.pop() {
            Some(n) => n,
            None if self.next >= self.limit => return None,
            None => {
                self.next += 1;
                self.next - 1
            }
        };
        self.allocated.push(n);
        Some(I::from(n))
    }

    /// Takes `id` out of the free list and the freed list, and keeps it:
    /// the engine still has the object, so nothing may be given the ID until
    /// `release`.
    pub fn hold(&mut self, id: I) {
        let n: u32 = id.into();
        if let Some(i) = self.free.iter().rposition(|f| *f == n) {
            self.free.remove(i);
        }
        if let Some(i) = self.freed.iter().rposition(|f| *f == n) {
            self.freed.remove(i);
        }
        if !self.held.contains(&n) {
            self.held.push(n);
        }
    }

    /// The held `id` is gone from the engine: it is free again.
    pub fn release(&mut self, id: I) {
        let n: u32 = id.into();
        if let Some(i) = self.held.iter().position(|h| *h == n) {
            self.held.remove(i);
            self.free(id);
        }
    }

    /// The held `id` stays live: its removal was dropped.
    pub fn unhold(&mut self, id: I) {
        let n: u32 = id.into();
        self.held.retain(|h| *h != n);
    }

    /// Whether `id` is held.
    pub fn is_held(&self, id: I) -> bool {
        self.held.contains(&id.into())
    }

    // how long the freed and allocated journals are
    fn marks(&self) -> (usize, usize) {
        (self.freed.len(), self.allocated.len())
    }

    // what the journals gained since `marks`
    fn since(&mut self, marks: (usize, usize)) -> (Vec<I>, Vec<I>) {
        let freed = self.freed.split_off(marks.0.min(self.freed.len()));
        let allocated = self.allocated.split_off(marks.1.min(self.allocated.len()));
        (
            freed.into_iter().map(I::from).collect(),
            allocated.into_iter().map(I::from).collect(),
        )
    }

    /// Returns `id` to the pool; the next `alloc` hands it out again.
    pub fn free(&mut self, id: I) {
        self.free.push(id.into());
        self.freed.push(id.into());
    }

    /// The IDs freed since the last call.
    pub fn take_freed(&mut self) -> Vec<I> {
        self.allocated.clear();
        self.freed.drain(..).map(I::from).collect()
    }

    /// How many IDs are allocated and not freed.
    pub fn live(&self) -> u32 {
        self.next - self.free.len() as u32
    }

    /// The most IDs that can be live at once.
    pub fn limit(&self) -> u32 {
        self.limit
    }
}

/// An ID from `pool`, or `LimitReached` naming `kind`.
pub(super) fn alloc<I: From<u32> + Into<u32> + Copy>(
    pool: &mut IdPool<I>,
    kind: &'static str,
) -> CmdResult<I> {
    pool.alloc().ok_or(CmdErr::LimitReached {
        kind,
        limit: pool.limit(),
    })
}

/// What a transport command acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Voice(VoiceId),
    Group(GroupId),
    Tempo(TempoId),
}

/// A kind of object `all` can stand for (REQ-006).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdKind {
    Voice,
    Group,
    Tempo,
}

/// Who owns a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProcOwner {
    Voice(VoiceId),
    Group(GroupId),
    /// No voice or group: a seq at the top level, which needs a pipe. It
    /// reads a clock (`main` by default), so it runs while that clock does.
    Session,
}

/// Whose transport runs a tempo: it ticks only while that object plays, and
/// starts/stops with it. A process's own tempo belongs to the process's owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TempoOwner {
    Voice(VoiceId),
    Group(GroupId),
    Context,
}

impl From<ProcOwner> for TempoOwner {
    fn from(owner: ProcOwner) -> Self {
        match owner {
            ProcOwner::Voice(v) => TempoOwner::Voice(v),
            ProcOwner::Group(g) => TempoOwner::Group(g),
            ProcOwner::Session => TempoOwner::Context,
        }
    }
}

/// The period of an object put on a clock without `in` (G5), in ticks.
pub const DEFAULT_PERIOD: u32 = 4;

/// The session's built-in clock (G2).
pub const MAIN: &str = "main";

/// Why an object uses the tempo it uses (REQ-011 item 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempoLink {
    /// A clock of its own (`on 93b`).
    Own,
    /// The default (G2): it follows its parent's clock (`main` at the top
    /// level) and adopts its group's clock when grouped.
    Parent,
    /// A named clock (`on name`).
    Context,
    /// A group's clock (`on group`, or adopted when grouped).
    Group(GroupId),
    /// Whatever its voice uses (a seq without `on`).
    Voice,
    /// A voice's clock (`on kick`): it moves when that voice moves.
    Follow(VoiceId),
}

impl TempoLink {
    /// True if an object with this link takes its group's tempo when it is
    /// grouped.
    pub fn follows_parent(self) -> bool {
        matches!(self, TempoLink::Parent)
    }
}

/// The ID pools, one per kind.
#[derive(Clone)]
pub struct Ids {
    pub(crate) voices: IdPool<VoiceId>,
    pub(crate) groups: IdPool<GroupId>,
    pub(crate) tempos: IdPool<TempoId>,
    pub(crate) procs: IdPool<ProcId>,
    pub(crate) tracks: IdPool<TrackId>,
    pub(crate) buffers: IdPool<BufferId>,
}

impl Ids {
    fn new(limits: &Limits) -> Self {
        Self {
            voices: IdPool::new(limits.voices),
            groups: IdPool::new(limits.groups),
            tempos: IdPool::new(limits.tempos),
            procs: IdPool::new(limits.procs),
            tracks: IdPool::new(limits.tracks),
            // buffers have no limit but memory (REQ-095)
            buffers: IdPool::new(u32::MAX),
        }
    }
}

/// Keeps track of all entities' states.
#[derive(Clone)]
pub struct EngineState {
    pub(crate) limits: Limits,
    pub(crate) ids: Ids,
    pub(crate) tracks: HashMap<String, TrackRepr>,
    /// Buffers by name (RECb).
    pub(crate) buffers: BTreeMap<String, BufferRepr>,
    /// Slots the engine's buffer table has room for.
    pub(crate) buffer_cap: usize,
    /// Takes on their way to a track or a file (RECc).
    pub(crate) saves: BufSaves,
    pub(crate) voices: BTreeMap<VoiceId, VoiceRepr>,
    /// Top-level voices only; grouped ones are in their group's `member_names`.
    pub(crate) voice_names: HashMap<String, VoiceId>,
    pub(crate) groups: BTreeMap<GroupId, GroupRepr>,
    pub(crate) group_names: HashMap<String, GroupId>,
    /// Every live tempo.
    pub(crate) tempos: BTreeMap<TempoId, TempoRepr>,
    pub(crate) context_names: HashMap<String, TempoId>,
    pub(crate) procs: BTreeMap<ProcId, ProcRepr>,
    /// The seqs without a voice or group (`ProcOwner::Session`), by name.
    pub(crate) session_procs: BTreeMap<String, ProcId>,
    pub(crate) packets: BTreeMap<PacketId, PacketRepr>,
    /// The next packet ID; never reused.
    pub(crate) next_packet: u32,
    /// Scheduled lines sent to the engine so far (SCHED).
    pub(crate) sched_sent: u64,
    /// Scheduled lines the engine has retired, as it last reported.
    pub(crate) sched_fired: u64,
    /// The scheduled lines the engine may still hold, newest last, with what
    /// each names. The engine drops one when something it names is removed
    /// (W103), and reports each line it retires by number.
    pub(crate) waiting: Vec<WaitingLine>,
    /// Warnings raised while a statement lowered (W102), taken by
    /// `settle_warnings`.
    pub(crate) warnings: Vec<(Code, String)>,
    /// Effects removed since `settle_warnings` (their ids are reused, so a
    /// waiting line that names one is dropped).
    /// The last take number given to a buffer's `start`.
    pub(crate) next_take: u32,
    pub(crate) freed_fx: Vec<(FxOwner, u16)>,
    /// The same for waves: (voice, id) freed since `settle_warnings`.
    pub(crate) freed_waves: Vec<(VoiceId, u8)>,
    /// True while a statement that waits is lowered (PKb2): it may make and
    /// remove objects, and may name one that a waiting line makes.
    pub(crate) staging: bool,
    /// What waiting statements did, until their entries are numbered.
    stage_log: Vec<StageRec>,
    /// Voices and groups whose removal waits (PKb3): they still exist, for
    /// the engine and for a line typed now, under their old names. The
    /// names are out of the maps, so a line that waits sees them gone.
    pub(crate) limbo: Vec<Limbo>,
    /// The removals the statement being staged marked.
    stage_marks: Vec<AnyId>,
    /// Texts the mirror has for the user, from events (a dropped line that
    /// had made or removed something); taken by `take_notes`.
    notes: Vec<String>,
    out_channels: usize,
    assets_dir: Option<PathBuf>,
}

/// A scheduled entry in the mirror: its number (the place it has in the
/// line's tree, which the engine's `ScheduleFired` repeats), the entry it is
/// nested in, the text of the line, and the objects it names, with their
/// names as they were when it was scheduled. `watched` is for a `thenif`: the
/// names of the glides it checks (`kick.gain`), in the order of its watch list.
#[derive(Clone, Debug, PartialEq)]
pub struct WaitingLine {
    pub(crate) seq_no: u64,
    pub(crate) parent: Option<u64>,
    pub(crate) text: String,
    pub(crate) ids: Vec<(AnyId, String)>,
    pub(crate) watched: Vec<String>,
    /// The waves a waiting `fm=` of this line will take as modulators.
    pub(crate) fm_ids: Vec<AnyId>,
    /// What the statements this entry holds made and removed in the mirror
    /// when they were lowered (PKb2); undone if the entry is dropped.
    pub(crate) staged: Vec<StageRec>,
}

/// The mirror as it was before a waiting statement lowered, for restoring
/// what the statement removed.
#[derive(Clone)]
pub(crate) struct Snapshot(Arc<EngineState>);

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Snapshot")
    }
}

impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What one waiting statement did to the mirror (PKb2): the objects it
/// made (they exist for the mirror, not yet for the engine) and the ones it
/// removed (gone for the mirror, their IDs held, still in the engine).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StageRec {
    /// The objects the statement's commands make or remove by name: how its
    /// entry finds the record.
    key: Vec<AnyId>,
    created: Vec<AnyId>,
    removed: Vec<AnyId>,
    /// The voices and groups whose removal the statement marked (PKb3): they
    /// stay in the mirror, nameable by a line typed now, until it runs.
    marked: Vec<AnyId>,
    /// The voices the statement moved into or out of a group, or into a new
    /// one: put back if the statement is dropped.
    edited: Vec<VoiceId>,
    /// The mirror before the statement, when it removed or moved something.
    before: Option<Snapshot>,
}

/// A voice or group whose removal waits (PKb3): its name, and the group it
/// is a member of.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Limbo {
    pub(crate) id: AnyId,
    pub(crate) name: String,
    pub(crate) group: Option<GroupId>,
    /// A group that dissolves (its members stay) rather than goes with them.
    pub(crate) dissolve: bool,
    /// For a seq, lfo or env: whose it is.
    pub(crate) owner: Option<ProcOwner>,
    /// For a seq of a packet that waits for its removal: the packet, which
    /// goes with the last of them.
    pub(crate) packet: Option<PacketId>,
}

/// The journals' lengths when a waiting statement began.
pub(crate) struct StageMark {
    pools: [(usize, usize); 6],
    freed_fx: usize,
    before: EngineState,
}

// the voices `cmd` moves into or out of a group, or into a new one
fn member_edits(cmd: &Command, out: &mut Vec<VoiceId>) {
    match cmd {
        Command::Group(a) => out.extend(a.members.iter().copied()),
        Command::GroupEdit(a) => {
            if let GroupOp::Join(v) | GroupOp::Leave(v) = a.op {
                out.push(v);
            }
        }
        Command::Schedule(a) => member_edits(&a.cmd, out),
        Command::Batch(a) => {
            for c in &a.cmds {
                member_edits(c, out);
            }
        }
        _ => {}
    }
}

/// How many `Schedule`s `cmd` holds, itself included, nested ones too.
pub(crate) fn schedule_count(cmd: &Command) -> u64 {
    match cmd {
        Command::Schedule(a) => 1 + schedule_count(&a.cmd),
        Command::Batch(a) => a.cmds.iter().map(schedule_count).sum(),
        _ => 0,
    }
}

// "voice" or "group.voice"
#[cfg(test)]
fn split_voice(name: &str) -> CmdResult<(Option<&str>, &str)> {
    let mut parts = name.split('.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(voice), None, _) => Ok((None, voice)),
        (Some(group), Some(voice), None) => Ok((Some(group), voice)),
        _ => Err(CmdErr::Formatting {
            err: "Too many delimiters for format group.voice".to_string(),
        }),
    }
}

#[cfg(test)]
fn no_voice(name: &str, group: Option<&str>) -> CmdErr {
    CmdErr::NoVoice {
        name: name.to_string(),
        group: group.map(str::to_string),
    }
}

fn no_item(ty: &str, name: &str) -> CmdErr {
    CmdErr::NoItem {
        ty: ty.to_string(),
        name: name.to_string(),
    }
}

impl EngineState {
    /// Tracks sorted by name, so `TrackId`s agree with `Conductor::prepare`;
    /// `Limits::default()`.
    pub fn new(files: Vec<AudioFile>, out_channels: usize) -> Self {
        Self::with_limits(files, out_channels, Limits::default())
    }

    /// Like `new`, with every ID pool sized by `limits`. The track limit is
    /// raised to the track count, as `Conductor::new` raises it.
    pub fn with_limits(files: Vec<AudioFile>, out_channels: usize, limits: Limits) -> Self {
        let mut limits = limits;
        limits.tracks = limits.tracks.max(files.len() as u32);
        let mut ids = Ids::new(&limits);

        let mut files = files;
        files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        let mut tracks = HashMap::with_capacity(files.len());
        for af in files {
            // the pool's limit is at least the track count
            let Some(id) = ids.tracks.alloc() else { break };
            let channels = af.num_channels;
            tracks.insert(
                af.file_name.clone(),
                TrackRepr::new(id, &af.file_name, channels, Arc::from(af.samples)),
            );
        }

        Self {
            limits,
            ids,
            tracks,
            buffers: BTreeMap::new(),
            buffer_cap: 0,
            saves: BufSaves::default(),
            voices: BTreeMap::new(),
            voice_names: HashMap::new(),
            groups: BTreeMap::new(),
            group_names: HashMap::new(),
            tempos: BTreeMap::new(),
            context_names: HashMap::new(),
            procs: BTreeMap::new(),
            session_procs: BTreeMap::new(),
            packets: BTreeMap::new(),
            next_packet: 0,
            sched_sent: 0,
            sched_fired: 0,
            waiting: Vec::new(),
            warnings: Vec::new(),
            next_take: 0,
            freed_fx: Vec::new(),
            freed_waves: Vec::new(),
            staging: false,
            stage_log: Vec::new(),
            limbo: Vec::new(),
            stage_marks: Vec::new(),
            notes: Vec::new(),
            out_channels,
            assets_dir: None,
        }
    }

    /// Scheduled lines the engine may still hold: sent, not yet reported
    /// fired. The engine reports a running total, so a lost event only
    /// makes this too high until the next one; it is never too low, and the
    /// engine's table (`Limits::scheduled`) never overflows.
    pub fn scheduled_pending(&self) -> u64 {
        self.sched_sent.saturating_sub(self.sched_fired)
    }

    /// Whether the scheduled entry with this number may still be in the
    /// engine's table: sent, and not reported retired.
    pub(crate) fn is_waiting(&self, seq_no: u64) -> bool {
        self.waiting.iter().any(|w| w.seq_no == seq_no)
    }

    /// Takes the engine's report of a retired scheduled line: its number,
    /// and the running total of retired lines. The total makes good a lost
    /// report; the list of waiting lines then keeps what it can't be sure
    /// of (so it may warn about a line that already fired, but never misses
    /// one) until nothing is pending.
    ///
    /// `applied` is false for a line the engine dropped. What the line made
    /// in the mirror is then forgotten, and what it removed comes back
    /// (PKb2); a line that ran lets go of the IDs of what it removed.
    pub fn note_schedule_fired(&mut self, seq_no: u64, total: u64, applied: bool) {
        let rec = self
            .waiting
            .iter()
            .position(|w| w.seq_no == seq_no)
            .map(|i| self.waiting.remove(i));
        self.sched_fired = self.sched_fired.max(total).min(self.sched_sent);
        if let Some(rec) = rec {
            if applied {
                for s in rec.staged {
                    self.finish_staged(&s);
                }
            } else {
                self.drop_record(rec);
            }
        }
        if self.scheduled_pending() == 0 {
            // nothing is left in the engine: what is still recorded ran, and
            // the report of it was lost
            for rec in std::mem::take(&mut self.waiting) {
                for s in rec.staged {
                    self.finish_staged(&s);
                }
            }
        }
        // IDs freed by all this: lines that name them are dropped
        let warns = self.settle_warnings();
        for (code, msg) in warns {
            self.push_note(format!("{code} {msg}"));
        }
    }

    /// The texts the mirror has for the user since the last call: lines
    /// that were dropped after they made or removed something.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn push_note(&mut self, text: String) {
        if !self.notes.contains(&text) {
            self.notes.push(text);
        }
    }

    // a line the engine dropped, already out of `waiting`
    fn drop_record(&mut self, rec: WaitingLine) {
        let held: Vec<&str> = rec
            .ids
            .iter()
            .filter(|(id, _)| self.is_held(*id))
            .map(|(_, name)| name.as_str())
            .collect();
        let held = held.join(", ");
        // due before the line that makes what it names
        let early: Vec<&str> = rec
            .ids
            .iter()
            .filter(|(id, _)| self.is_pending(*id))
            .map(|(_, name)| name.as_str())
            .collect();
        let early = early.join(", ");
        let mut effects = Vec::new();
        for s in rec.staged.into_iter().rev() {
            effects.extend(self.undo(s));
        }
        let code = Code::WaitingLineDropped;
        if !effects.is_empty() {
            let why = effects.join(", ");
            self.push_note(format!(
                "{code} the waiting line '{}' did not run: {why}",
                rec.text
            ));
        } else if !held.is_empty() && rec.watched.is_empty() {
            self.push_note(format!(
                "{code} the waiting line '{}' is dropped (it names {held})",
                rec.text
            ));
        } else if !early.is_empty() && rec.watched.is_empty() {
            self.push_note(format!(
                "{code} the waiting line '{}' is dropped ({early} didn't exist yet when it was due)",
                rec.text
            ));
        }
    }

    /// Whether `id` is the ID of an object a waiting line will remove.
    pub(crate) fn is_held(&self, id: AnyId) -> bool {
        if self.in_limbo(id) {
            return true;
        }
        match id {
            AnyId::Voice(v) => self.ids.voices.is_held(v),
            AnyId::Group(g) => self.ids.groups.is_held(g),
            AnyId::Tempo(t) => self.ids.tempos.is_held(t),
            AnyId::Proc(p) => self.ids.procs.is_held(p),
            AnyId::Track(t) => self.ids.tracks.is_held(t),
            AnyId::Buffer(b) => self.ids.buffers.is_held(b),
            AnyId::Fx { .. } | AnyId::Wave { .. } => false,
        }
    }

    /// Whether `id` names an object that a waiting line will make but that
    /// the engine does not have yet. A statement typed now can't name it
    /// (E411); a waiting one may.
    /// The waiting line that will make wave `id` a modulator (`fm=`), if any:
    /// the wave can't be removed before it runs.
    pub(crate) fn waiting_fm_line(&self, id: AnyId) -> Option<&WaitingLine> {
        self.waiting.iter().find(|w| w.fm_ids.contains(&id))
    }

    pub(crate) fn is_pending(&self, id: AnyId) -> bool {
        self.stage_log.iter().any(|r| r.created.contains(&id))
            || self
                .waiting
                .iter()
                .any(|w| w.staged.iter().any(|r| r.created.contains(&id)))
    }

    // ---- staged creation and removal (PKb2) ----

    /// Starts the record of a statement that waits: remembers the mirror, and
    /// how far the ID journals are.
    pub(crate) fn stage_begin(&mut self) -> StageMark {
        let mut before = self.clone();
        // a snapshot holds no records of its own
        before.waiting.clear();
        before.stage_log.clear();
        before.notes.clear();
        let ids = &self.ids;
        StageMark {
            pools: [
                ids.voices.marks(),
                ids.groups.marks(),
                ids.tempos.marks(),
                ids.procs.marks(),
                ids.tracks.marks(),
                ids.buffers.marks(),
            ],
            freed_fx: self.freed_fx.len(),
            before,
        }
    }

    /// Ends the record: what the statement's `cmds` made and removed is
    /// filed until its entry is numbered. Removed IDs are held from now.
    pub(crate) fn stage_end(&mut self, mark: StageMark, cmds: &[Command]) {
        let StageMark {
            pools,
            freed_fx,
            before,
        } = mark;
        let ids = &mut self.ids;
        let mut freed: Vec<AnyId> = Vec::new();
        let mut made: Vec<AnyId> = Vec::new();
        let (f, a) = ids.voices.since(pools[0]);
        freed.extend(f.into_iter().map(AnyId::Voice));
        made.extend(a.into_iter().map(AnyId::Voice));
        let (f, a) = ids.groups.since(pools[1]);
        freed.extend(f.into_iter().map(AnyId::Group));
        made.extend(a.into_iter().map(AnyId::Group));
        let (f, a) = ids.tempos.since(pools[2]);
        freed.extend(f.into_iter().map(AnyId::Tempo));
        made.extend(a.into_iter().map(AnyId::Tempo));
        let (f, a) = ids.procs.since(pools[3]);
        freed.extend(f.into_iter().map(AnyId::Proc));
        made.extend(a.into_iter().map(AnyId::Proc));
        let (f, a) = ids.tracks.since(pools[4]);
        freed.extend(f.into_iter().map(AnyId::Track));
        made.extend(a.into_iter().map(AnyId::Track));
        let (f, a) = ids.buffers.since(pools[5]);
        freed.extend(f.into_iter().map(AnyId::Buffer));
        made.extend(a.into_iter().map(AnyId::Buffer));
        let _ = freed_fx;

        // made and freed inside one statement is nothing
        let removed: Vec<AnyId> = freed
            .iter()
            .copied()
            .filter(|i| !made.contains(i))
            .collect();
        let mut created: Vec<AnyId> = made
            .iter()
            .copied()
            .filter(|i| !freed.contains(i))
            .collect();
        for cmd in cmds {
            created_ids(cmd, &mut |id| {
                if matches!(id, AnyId::Fx { .. } | AnyId::Wave { .. }) && !created.contains(&id) {
                    created.push(id);
                }
            });
        }
        let mut key = Vec::new();
        for cmd in cmds {
            effect_ids(cmd, &mut key);
        }
        let marked = std::mem::take(&mut self.stage_marks);
        let mut edited = Vec::new();
        for cmd in cmds {
            member_edits(cmd, &mut edited);
        }
        edited.retain(|v| before.voices.contains_key(v));
        if created.is_empty() && removed.is_empty() && marked.is_empty() && edited.is_empty() {
            return;
        }
        for id in &removed {
            self.hold(*id);
        }
        let keep = !removed.is_empty() || !edited.is_empty();
        let before = keep.then(|| Snapshot(Arc::new(before)));
        self.stage_log.push(StageRec {
            key,
            created,
            removed,
            marked,
            edited,
            before,
        });
    }

    fn hold(&mut self, id: AnyId) {
        let ids = &mut self.ids;
        match id {
            AnyId::Voice(v) => ids.voices.hold(v),
            AnyId::Group(g) => ids.groups.hold(g),
            AnyId::Tempo(t) => ids.tempos.hold(t),
            AnyId::Proc(p) => ids.procs.hold(p),
            AnyId::Track(t) => ids.tracks.hold(t),
            AnyId::Buffer(b) => ids.buffers.hold(b),
            AnyId::Fx { .. } | AnyId::Wave { .. } => {}
        }
    }

    fn release(&mut self, id: AnyId) {
        let ids = &mut self.ids;
        match id {
            AnyId::Voice(v) => ids.voices.release(v),
            AnyId::Group(g) => ids.groups.release(g),
            AnyId::Tempo(t) => ids.tempos.release(t),
            AnyId::Proc(p) => ids.procs.release(p),
            AnyId::Track(t) => ids.tracks.release(t),
            AnyId::Buffer(b) => ids.buffers.release(b),
            AnyId::Fx { .. } | AnyId::Wave { .. } => {}
        }
    }

    fn unhold(&mut self, id: AnyId) {
        let ids = &mut self.ids;
        match id {
            AnyId::Voice(v) => ids.voices.unhold(v),
            AnyId::Group(g) => ids.groups.unhold(g),
            AnyId::Tempo(t) => ids.tempos.unhold(t),
            AnyId::Proc(p) => ids.procs.unhold(p),
            AnyId::Track(t) => ids.tracks.unhold(t),
            AnyId::Buffer(b) => ids.buffers.unhold(b),
            AnyId::Fx { .. } | AnyId::Wave { .. } => {}
        }
    }

    // the statement ran in the engine: the IDs it removed are free, and the
    // voices and groups it marked go for real
    fn finish_staged(&mut self, rec: &StageRec) {
        for id in &rec.removed {
            self.release(*id);
        }
        for id in &rec.marked {
            self.apply_removal(*id);
        }
    }

    // gives the records in the log that a Schedule with these own effects
    // holds to the entry
    fn claim_staged(&mut self, own: &[AnyId]) -> Vec<StageRec> {
        if own.is_empty() || self.stage_log.is_empty() {
            return Vec::new();
        }
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.stage_log)
            .into_iter()
            .partition(|r| r.key.iter().any(|k| own.contains(k)));
        self.stage_log = rest;
        mine
    }

    /// Ends a line's numbering: a record that no entry claimed belongs to
    /// nothing that waits, so it ran.
    pub(crate) fn stage_done(&mut self) {
        for rec in std::mem::take(&mut self.stage_log) {
            self.finish_staged(&rec);
        }
    }

    // Takes back what a dropped statement did to the mirror: forgets the
    // objects it made, brings back the ones it removed. Answers what it did,
    // for the message.
    fn undo(&mut self, rec: StageRec) -> Vec<String> {
        let mut said = Vec::new();
        // voices moved into or out of groups go back first
        if let Some(Snapshot(before)) = &rec.before {
            for v in rec.edited.iter().rev() {
                self.restore_membership(*v, before);
            }
        }
        // removals that waited are off
        for id in rec.marked.iter().rev() {
            let name = self.name_of(*id);
            if self.unmark(*id) {
                said.push(format!("{name} stays"));
            }
        }
        // the last made goes first
        for id in rec.created.iter().rev() {
            let name = self.name_of(*id);
            if self.forget_object(*id) {
                said.push(format!("{name} was not created"));
            }
        }
        if let Some(Snapshot(before)) = &rec.before {
            // owners before what they own
            let rank = |id: &AnyId| match id {
                AnyId::Voice(_) | AnyId::Group(_) => 0,
                AnyId::Tempo(_) => 1,
                _ => 2,
            };
            let mut order = rec.removed.clone();
            order.sort_by_key(rank);
            for id in &order {
                let name = before.name_of(*id);
                if self.restore_object(*id, before) {
                    said.push(format!("{name} stays"));
                } else {
                    self.release(*id);
                }
            }
        }
        said
    }

    // The ID `id` belongs to a made object that will never exist. If a later
    // waiting removal holds it, that removal has nothing to remove or restore.
    fn free_made(&mut self, id: AnyId) {
        if self.is_held(id) {
            for w in &mut self.waiting {
                for s in &mut w.staged {
                    s.removed.retain(|r| *r != id);
                }
            }
            for s in &mut self.stage_log {
                s.removed.retain(|r| *r != id);
            }
            self.unhold(id);
        }
        let ids = &mut self.ids;
        match id {
            AnyId::Voice(v) => ids.voices.free(v),
            AnyId::Group(g) => ids.groups.free(g),
            AnyId::Tempo(t) => ids.tempos.free(t),
            AnyId::Proc(p) => ids.procs.free(p),
            AnyId::Track(t) => ids.tracks.free(t),
            AnyId::Buffer(b) => ids.buffers.free(b),
            AnyId::Fx { owner, id } => self.freed_fx.push((owner, id)),
            AnyId::Wave { voice, id } => self.freed_waves.push((voice, id)),
        }
    }

    // takes a made object out of the mirror and frees its ID; true if it
    // was there
    pub(crate) fn forget_object(&mut self, id: AnyId) -> bool {
        let there = match id {
            AnyId::Voice(v) => match self.voices.remove(&v) {
                None => false,
                Some(repr) => {
                    match repr.group.and_then(|g| self.groups.get_mut(&g)) {
                        Some(group) => {
                            group.member_names.remove(&repr.name);
                            group.members.retain(|m| *m != v);
                        }
                        None => {
                            if self.voice_names.get(&repr.name) == Some(&v) {
                                self.voice_names.remove(&repr.name);
                            }
                        }
                    }
                    self.drop_packets_of(ProcOwner::Voice(v));
                    // a pipe that named it stops naming it
                    self.strip_receivers(ProcOwner::Voice(v), &repr.name);
                    true
                }
            },
            AnyId::Tempo(t) => {
                let was = self.tempos.remove(&t).is_some();
                self.context_names.retain(|_, c| *c != t);
                was
            }
            AnyId::Proc(p) => match self.procs.remove(&p) {
                None => false,
                Some(repr) => {
                    if let Some(procs) = self.owner_procs_mut(repr.owner)
                        && procs.get(&repr.name) == Some(&p)
                    {
                        procs.remove(&repr.name);
                    }
                    if let Some(packet) = repr.packet.and_then(|k| self.packets.get_mut(&k)) {
                        packet.seqs.retain(|s| *s != p);
                    }
                    true
                }
            },
            AnyId::Fx { owner, id: fx } => {
                let was = self.fx_list_mut(owner).is_some_and(|(list, _)| {
                    let n = list.len();
                    list.retain(|f| f.id != fx);
                    list.len() < n
                });
                if was {
                    self.freed_fx.push((owner, fx));
                }
                return was;
            }
            AnyId::Wave { voice, id: wave } => {
                let was = self.voices.get_mut(&voice).is_some_and(|r| {
                    let n = r.waves.len();
                    r.waves.retain(|w| w.id != wave);
                    // whatever followed it follows nothing, as in the engine
                    for w in &mut r.waves {
                        w.fm &= !(1 << wave);
                    }
                    r.waves.len() < n
                });
                if was {
                    self.freed_waves.push((voice, wave));
                }
                return was;
            }
            AnyId::Group(g) => match self.groups.remove(&g) {
                None => false,
                Some(repr) => {
                    if self.group_names.get(&repr.name) == Some(&g) {
                        self.group_names.remove(&repr.name);
                    }
                    for p in repr.procs.values() {
                        if self.procs.remove(p).is_some() {
                            self.ids.procs.free(*p);
                        }
                    }
                    self.drop_packets_of(ProcOwner::Group(g));
                    let _ = self.strip_receivers(ProcOwner::Group(g), &repr.name);
                    true
                }
            },
            // not staged
            AnyId::Track(_) | AnyId::Buffer(_) => false,
        };
        self.limbo.retain(|l| l.id != id);
        // gone already: a later waiting removal may hold the ID
        if there || self.is_held(id) {
            self.free_made(id);
        }
        there
    }

    // Puts a clock that a dropped line freed (a member's own, when a group
    // took it over) back from `before`; false if it can't be.
    fn restore_object(&mut self, id: AnyId, before: &EngineState) -> bool {
        match id {
            AnyId::Tempo(t) => {
                let Some(repr) = before.tempos.get(&t).cloned() else {
                    return false;
                };
                if self.tempos.contains_key(&t) {
                    return false;
                }
                // a voice or seq that was removed with it owns it again
                self.tempos.insert(t, repr);
                for (name, c) in &before.context_names {
                    if *c == t && !self.context_names.contains_key(name) {
                        self.context_names.insert(name.clone(), t);
                    }
                }
                self.unhold(id);
                true
            }
            // voices, groups, seqs and clocks that wait for their removal
            // are marked, not removed (PKb3)
            AnyId::Voice(_) | AnyId::Proc(_) => false,
            AnyId::Group(_)
            | AnyId::Track(_)
            | AnyId::Buffer(_)
            | AnyId::Fx { .. }
            | AnyId::Wave { .. } => false,
        }
    }

    // ---- removals that wait: voices and groups stay nameable (PKb3) ----

    /// Whether the removal of `id` (a voice or group) waits.
    pub(crate) fn in_limbo(&self, id: AnyId) -> bool {
        self.limbo.iter().any(|l| l.id == id)
    }

    /// Whether a line that waits can't see group `g`: its removal waits.
    pub(crate) fn hides_group(&self, g: GroupId) -> bool {
        self.staging
            && self
                .limbo
                .iter()
                .any(|l| l.id == AnyId::Group(g) && !l.dissolve)
    }

    /// Whether a line that waits can see `voice`: its removal, and its
    /// group's, are not waiting. A line typed now sees every voice.
    pub(crate) fn sees_voice(&self, voice: &VoiceRepr) -> bool {
        !self.staging
            || !(self.in_limbo(AnyId::Voice(voice.id))
                || voice.group.is_some_and(|g| self.hides_group(g)))
    }

    /// The voice called `name` whose removal waits (in group `group`, or
    /// anywhere when `None`).
    pub(crate) fn limbo_voice(
        &self,
        name: &str,
        group: Option<Option<GroupId>>,
    ) -> Option<VoiceId> {
        self.limbo.iter().find_map(|l| match l.id {
            AnyId::Voice(v) if l.name == name && group.is_none_or(|g| g == l.group) => Some(v),
            _ => None,
        })
    }

    /// The group called `name` whose removal waits.
    /// The seq, lfo or env `name` of `owner` whose removal waits.
    pub(crate) fn limbo_proc(&self, owner: ProcOwner, name: &str) -> Option<ProcId> {
        self.limbo.iter().find_map(|l| match l.id {
            AnyId::Proc(p) if l.name == name && l.owner == Some(owner) => Some(p),
            _ => None,
        })
    }

    /// The clock called `name` whose removal waits.
    pub(crate) fn limbo_tempo(&self, name: &str) -> Option<TempoId> {
        self.limbo.iter().find_map(|l| match l.id {
            AnyId::Tempo(t) if l.name == name => Some(t),
            _ => None,
        })
    }

    /// Whether a line that waits can't see `id` (a seq, lfo, env or clock):
    /// its removal waits.
    pub(crate) fn hidden(&self, id: AnyId) -> bool {
        self.staging && self.in_limbo(id)
    }

    /// The seq, lfo or env `name` of `owner` as the line being lowered sees
    /// it: a line typed now sees one whose removal waits, a line that waits
    /// does not.
    pub(crate) fn proc_named(&self, owner: ProcOwner, name: &str) -> Option<ProcId> {
        if !self.staging
            && let Some(p) = self.limbo_proc(owner, name)
        {
            return Some(p);
        }
        let p = *self.owner_procs(owner)?.get(name)?;
        (!self.hidden(AnyId::Proc(p))).then_some(p)
    }

    /// The clock `name` as the line being lowered sees it.
    pub(crate) fn clock_named(&self, name: &str) -> Option<TempoId> {
        if !self.staging
            && let Some(t) = self.limbo_tempo(name)
        {
            return Some(t);
        }
        let t = *self.context_names.get(name)?;
        (!self.hidden(AnyId::Tempo(t))).then_some(t)
    }

    /// Whether a line that waits can't see the packet `pk`: every seq it
    /// has waits for its removal.
    pub(crate) fn hides_packet(&self, pk: PacketId) -> bool {
        self.staging && self.limbo.iter().any(|l| l.packet == Some(pk))
    }

    /// Removes a seq, lfo or env, or marks it for removal when a line that
    /// waits does it.
    pub(crate) fn remove_proc_view(&mut self, id: ProcId) -> Command {
        if self.staging {
            self.staged_removal(AnyId::Proc(id), false, |es| Ok(es.remove_proc(id)))
                .unwrap_or_else(|_| Command::Batch(BatchArgs { cmds: Vec::new() }))
        } else {
            self.remove_proc(id)
        }
    }

    /// The group called `name` whose removal waits.
    pub(crate) fn limbo_group(&self, name: &str) -> Option<GroupId> {
        self.limbo.iter().find_map(|l| match l.id {
            AnyId::Group(g) if l.name == name => Some(g),
            _ => None,
        })
    }

    /// Runs `remove`, which takes `root` (a voice or group) and what goes
    /// with it out of the mirror and answers the commands that do the same in
    /// the engine, for a removal that waits: the mirror is put back as it
    /// was, except that `root` is marked. Its name is out of the maps (a line
    /// that waits sees it gone and may use the name), and it is removed for
    /// real when the line runs, or stays if the line is dropped.
    pub(crate) fn staged_removal<R>(
        &mut self,
        root: AnyId,
        dissolve: bool,
        remove: impl FnOnce(&mut Self) -> CmdResult<R>,
    ) -> CmdResult<R> {
        let saved = self.clone();
        let out = remove(self);
        *self = saved;
        if out.is_ok() {
            self.mark_limbo(root, dissolve);
        }
        out
    }

    fn mark_limbo(&mut self, root: AnyId, dissolve: bool) {
        let (name, group) = match root {
            AnyId::Voice(v) => {
                let Some(repr) = self.voices.get(&v) else {
                    return;
                };
                let (name, group) = (repr.name.clone(), repr.group);
                match group.and_then(|g| self.groups.get_mut(&g)) {
                    Some(g) => {
                        g.member_names.remove(&name);
                    }
                    None => {
                        if self.voice_names.get(&name) == Some(&v) {
                            self.voice_names.remove(&name);
                        }
                    }
                }
                (name, group)
            }
            AnyId::Group(g) => {
                let Some(repr) = self.groups.get(&g) else {
                    return;
                };
                let name = repr.name.clone();
                if self.group_names.get(&name) == Some(&g) {
                    self.group_names.remove(&name);
                }
                (name, None)
            }
            AnyId::Proc(p) => {
                let Some(repr) = self.procs.get(&p) else {
                    return;
                };
                let (name, owner) = (repr.name.clone(), repr.owner);
                self.limbo.push(Limbo {
                    id: root,
                    name,
                    group: None,
                    dissolve: false,
                    owner: Some(owner),
                    packet: None,
                });
                self.stage_marks.push(root);
                return;
            }
            AnyId::Tempo(t) => {
                // an unnamed clock is a voice's or seq's own: it goes with its owner
                let Some(name) = self
                    .context_names
                    .iter()
                    .find(|(_, id)| **id == t)
                    .map(|(n, _)| n.clone())
                else {
                    return;
                };
                self.limbo.push(Limbo {
                    id: root,
                    name,
                    group: None,
                    dissolve: false,
                    owner: None,
                    packet: None,
                });
                self.stage_marks.push(root);
                return;
            }
            _ => return,
        };
        self.limbo.push(Limbo {
            id: root,
            name,
            group,
            dissolve,
            owner: None,
            packet: None,
        });
        self.stage_marks.push(root);
    }

    // The removal of `id` is off: it takes its name back, or `name_` if
    // another object has it now. False if it was not waiting.
    fn unmark(&mut self, id: AnyId) -> bool {
        let Some(i) = self.limbo.iter().position(|l| l.id == id) else {
            return false;
        };
        let l = self.limbo.remove(i);
        match id {
            AnyId::Voice(v) => {
                if !self.voices.contains_key(&v) {
                    return false;
                }
                let group = l.group.filter(|g| self.groups.contains_key(g));
                let name = self.free_name(&l.name, group);
                match group.and_then(|g| self.groups.get_mut(&g)) {
                    Some(g) => {
                        g.member_names.insert(name.clone(), v);
                    }
                    None => {
                        self.voice_names.insert(name.clone(), v);
                    }
                }
                if let Some(r) = self.voices.get_mut(&v) {
                    r.name = name;
                }
                true
            }
            AnyId::Group(g) => {
                if !self.groups.contains_key(&g) {
                    return false;
                }
                let name = self.free_name(&l.name, None);
                self.group_names.insert(name.clone(), g);
                if let Some(r) = self.groups.get_mut(&g) {
                    r.name = name;
                }
                true
            }
            AnyId::Proc(p) => {
                let Some(owner) = l.owner else {
                    return false;
                };
                if !self.procs.contains_key(&p) {
                    return false;
                }
                // a new seq may have the name by now
                let mut name = l.name.clone();
                if let Some(procs) = self.owner_procs_mut(owner)
                    && procs.get(&name) != Some(&p)
                {
                    while procs.contains_key(&name) {
                        name.push('_');
                    }
                    procs.insert(name.clone(), p);
                }
                if let Some(r) = self.procs.get_mut(&p) {
                    r.name = name;
                }
                true
            }
            AnyId::Tempo(t) => {
                if !self.tempos.contains_key(&t) {
                    return false;
                }
                if self.context_names.get(&l.name) != Some(&t) {
                    let name = self.free_name(&l.name, None);
                    self.context_names.insert(name, t);
                }
                true
            }
            _ => false,
        }
    }

    // The waiting removal of `id` ran in the engine: it goes from the mirror
    // too. Its warnings were said when the line was typed.
    fn apply_removal(&mut self, id: AnyId) {
        if !self.in_limbo(id) {
            return;
        }
        let warnings = std::mem::take(&mut self.warnings);
        let dissolve = self.limbo.iter().any(|l| l.id == id && l.dissolve);
        match id {
            AnyId::Voice(v) => {
                let _ = self.unload_voice(v);
            }
            AnyId::Group(g) if dissolve => {
                let _ = super::group::dissolve_group_in(self, g);
            }
            AnyId::Group(g) => {
                let _ = super::group::remove_group_in(self, g);
            }
            AnyId::Proc(p) => {
                let packet = self
                    .limbo
                    .iter()
                    .find(|l| l.id == id)
                    .and_then(|l| l.packet);
                let _ = self.remove_proc(p);
                if let Some(pk) = packet
                    && self.packets.get(&pk).is_some_and(|k| k.seqs.is_empty())
                {
                    self.packets.remove(&pk);
                }
            }
            AnyId::Tempo(t) => {
                self.tempos.remove(&t);
                self.context_names.retain(|_, c| *c != t);
                self.ids.tempos.free(t);
            }
            _ => {}
        }
        self.limbo.retain(|l| l.id != id);
        self.warnings = warnings;
    }

    /// Removes a voice with what it owns, or marks it for removal when a
    /// line that waits does it (`unload_voice`).
    pub(crate) fn remove_voice(&mut self, voice: VoiceId) -> CmdResult<Command> {
        if self.staging {
            self.staged_removal(AnyId::Voice(voice), false, |es| es.unload_voice(voice))
        } else {
            self.unload_voice(voice)
        }
    }

    // Puts a voice's group, name and clocks back as `before` had them, after
    // a dropped line moved it.
    fn restore_membership(&mut self, v: VoiceId, before: &EngineState) {
        let (Some(b), Some(cur)) = (before.voices.get(&v), self.voices.get(&v)) else {
            return;
        };
        let cur_name = cur.name.clone();
        match cur.group.and_then(|g| self.groups.get_mut(&g)) {
            Some(g) => {
                g.members.retain(|m| *m != v);
                g.member_names.remove(&cur_name);
            }
            None => {
                if self.voice_names.get(&cur_name) == Some(&v) {
                    self.voice_names.remove(&cur_name);
                }
            }
        }
        let group = b.group.filter(|g| self.groups.contains_key(g));
        let name = self.free_name(&b.name, group);
        match group.and_then(|g| self.groups.get_mut(&g)) {
            Some(g) => {
                g.member_names.insert(name.clone(), v);
                if !g.members.contains(&v) {
                    // where it was, as far as the group still has room
                    let at = group
                        .and_then(|g| before.groups.get(&g))
                        .and_then(|g| g.members.iter().position(|m| *m == v))
                        .unwrap_or(usize::MAX)
                        .min(g.members.len());
                    g.members.insert(at, v);
                }
            }
            None => {
                self.voice_names.insert(name.clone(), v);
            }
        }
        let procs: Vec<ProcId> = self
            .voices
            .get(&v)
            .map(|r| r.procs.values().copied().collect())
            .unwrap_or_default();
        if let Some(r) = self.voices.get_mut(&v) {
            r.group = group;
            r.name = name;
            r.link = b.link;
            r.tempo = b.tempo;
            r.active = b.active;
        }
        for p in procs {
            if let (Some(now), Some(was)) = (self.procs.get_mut(&p), before.procs.get(&p)) {
                now.tempo = was.tempo;
                now.link = was.link;
            }
        }
    }

    // `name`, or `name_` and so on if another voice has it
    fn free_name(&self, name: &str, group: Option<GroupId>) -> String {
        let mut name = name.to_string();
        loop {
            let taken = match group.and_then(|g| self.groups.get(&g)) {
                Some(g) => g.member_names.contains_key(&name),
                None => {
                    self.voice_names.contains_key(&name)
                        || self.context_names.contains_key(&name)
                        || self.group_names.contains_key(&name)
                }
            };
            if !taken {
                return name;
            }
            name.push('_');
        }
    }

    /// Records the scheduled entries of a line that is being sent. `cmd` is
    /// the line's command (a `Schedule`, a `Batch` of them, or any command
    /// that holds some). Every `Schedule` in the tree gets the next number,
    /// outermost first, and a record of what it names.
    pub(crate) fn note_scheduled(&mut self, text: &str, cmd: &mut Command) {
        self.note_scheduled_in(text, cmd, None);
        self.stage_done();
    }

    fn note_scheduled_in(&mut self, text: &str, cmd: &mut Command, parent: Option<u64>) {
        match cmd {
            Command::Schedule(_) => {
                let seq_no = self.note_entry(text, cmd, parent);
                if let Command::Schedule(args) = cmd {
                    self.note_scheduled_in(text, &mut args.cmd, Some(seq_no));
                }
            }
            Command::Batch(args) => {
                for c in &mut args.cmds {
                    self.note_scheduled_in(text, c, parent);
                }
            }
            _ => {}
        }
    }

    /// Records the entries of a script segment sent early (PKb): `cmd` is
    /// its outer `Schedule` (due `AfterMark`), whose body is a `Batch` with
    /// one nested entry per line of the segment, in order; `texts` are those
    /// lines as written. Numbers like `note_scheduled`, but each line's
    /// records carry that line's text, so a dropped line is named. Returns
    /// the outer entry's number.
    pub(crate) fn note_segment(
        &mut self,
        wait_text: &str,
        texts: &[String],
        cmd: &mut Command,
    ) -> u64 {
        let seq_no = self.note_entry(wait_text, cmd, None);
        let Command::Schedule(args) = cmd else {
            return seq_no;
        };
        let Command::Batch(body) = &mut *args.cmd else {
            return seq_no;
        };
        let mut texts = texts.iter();
        for c in &mut body.cmds {
            let text = if matches!(c, Command::Schedule(_)) {
                texts.next().map_or(wait_text, String::as_str)
            } else {
                wait_text
            };
            self.note_scheduled_in(text, c, Some(seq_no));
        }
        self.stage_done();
        seq_no
    }

    // the record of one `Schedule`: gives it the next number
    fn note_entry(&mut self, text: &str, cmd: &mut Command, parent: Option<u64>) -> u64 {
        let mut found = Vec::new();
        let free = matches!(&*cmd, Command::Schedule(a)
            if !matches!(a.when, WhenSpec::AfterGlides { watch: Some(_) })
                && super::holds_no_statement(&a.cmd));
        if free {
            // a head, tail or wait names only its clock: the statements
            // nested in it have records of their own
            if let Command::Schedule(a) = &*cmd
                && let WhenSpec::At { tempo, .. }
                | WhenSpec::After { tempo, .. }
                | WhenSpec::AfterMark { tempo, .. } = a.when
            {
                found.push(AnyId::Tempo(tempo));
            }
        } else {
            visit_ids(cmd, &mut |id| {
                if !found.contains(&id) {
                    found.push(id);
                }
            });
        }
        let ids = found.into_iter().map(|id| (id, self.name_of(id))).collect();
        let Command::Schedule(args) = cmd else {
            return self.sched_sent;
        };
        let mut own = Vec::new();
        effect_ids(&args.cmd, &mut own);
        let staged = self.claim_staged(&own);
        let mut fm_ids = Vec::new();
        fm_modulator_ids(&args.cmd, &mut fm_ids);
        let watched = match &args.when {
            WhenSpec::AfterGlides { watch: Some(w) } => {
                w.iter().map(|w| self.glide_name(w.glide)).collect()
            }
            _ => Vec::new(),
        };
        let seq_no = self.sched_sent;
        args.seq_no = seq_no;
        self.waiting.push(WaitingLine {
            seq_no,
            parent,
            text: text.to_string(),
            ids,
            watched,
            fm_ids,
            staged,
        });
        self.sched_sent += 1;
        seq_no
    }

    /// What a glide is called in a message: `kick.gain`, `drums.gain`.
    pub(crate) fn glide_name(&self, glide: GlideRef) -> String {
        match glide {
            GlideRef::Voice { voice, param } => {
                format!("{}.{}", self.name_of(AnyId::Voice(voice)), param.name())
            }
            GlideRef::Group(g) => format!("{}.gain", self.name_of(AnyId::Group(g))),
            GlideRef::Tempo(t) => format!("{}.rate", self.name_of(AnyId::Tempo(t))),
            GlideRef::Fx { owner, id, param } => {
                let owner_name = match owner {
                    FxOwner::Voice(v) => self.name_of(AnyId::Voice(v)),
                    FxOwner::Group(g) => self.name_of(AnyId::Group(g)),
                };
                let fx = self.name_of(AnyId::Fx { owner, id });
                let param = self
                    .fx_list(owner)
                    .iter()
                    .find(|f| f.id == id)
                    .and_then(|f| own_settings(f.kind.ty).get(usize::from(param)))
                    .map_or("?", |s| s.name);
                format!("{owner_name}.{fx}.{param}")
            }
            GlideRef::Wave { voice, id, param } => {
                let owner = self.name_of(AnyId::Voice(voice));
                let wave = self.name_of(AnyId::Wave { voice, id });
                let param = WaveParam::from_index(param).map_or("?", WaveParam::name);
                format!("{owner}.{wave}.{param}")
            }
            GlideRef::Proc(p) => self.name_of(AnyId::Proc(p)),
        }
    }

    /// The W105 text for a `thenif` entry the engine dropped (the engine
    /// reports `watch`: the index of the glide that was cut short, or
    /// `u16::MAX` when an object is gone). Asked before the entry's
    /// `ScheduleFired` is fed to the mirror.
    pub fn chain_dropped_text(&self, seq_no: u64, watch: u16) -> String {
        let tail = "the rest of the line was dropped";
        let line = self.waiting.iter().find(|w| w.seq_no == seq_no);
        let why = match (line, watch) {
            (Some(w), i) if i != u16::MAX && usize::from(i) < w.watched.len() => {
                format!("the glide on {} was cut short", w.watched[usize::from(i)])
            }
            (Some(w), _) if !w.watched.is_empty() => {
                format!("{} is gone", w.watched.join(", "))
            }
            _ => "a glide was cut short".to_string(),
        };
        format!("{} thenif: {why}; {tail}", Code::ChainCutShort)
    }

    /// What the object `id` is called, for a message.
    pub(crate) fn name_of(&self, id: AnyId) -> String {
        let named = match id {
            AnyId::Voice(v) => self.display_name(v),
            AnyId::Group(g) => self.groups.get(&g).map(|g| g.name.clone()),
            AnyId::Tempo(t) => self
                .context_names
                .iter()
                .find(|(_, id)| **id == t)
                .map(|(name, _)| name.clone())
                .or_else(|| match self.tempos.get(&t)?.owner {
                    TempoOwner::Voice(v) => self.display_name(v),
                    TempoOwner::Group(g) => self.groups.get(&g).map(|g| g.name.clone()),
                    TempoOwner::Context => None,
                }),
            AnyId::Proc(p) => self.proc_path(p),
            AnyId::Fx { owner, id } => self
                .fx_list(owner)
                .iter()
                .find(|f| f.id == id)
                .map(|f| f.name.clone()),
            AnyId::Wave { voice, id } => self
                .wave_list(voice)
                .iter()
                .find(|w| w.id == id)
                .map(|w| w.name.clone()),
            AnyId::Track(t) => self
                .tracks
                .values()
                .find(|r| r.id == t)
                .map(|r| r.name.clone()),
            AnyId::Buffer(b) => self
                .buffers
                .values()
                .find(|r| r.id == b)
                .map(|r| r.name.clone()),
        };
        named.unwrap_or_else(|| match id {
            AnyId::Voice(v) => format!("voice #{}", v.0),
            AnyId::Group(g) => format!("group #{}", g.0),
            AnyId::Tempo(t) => format!("clock #{}", t.0),
            AnyId::Proc(p) => format!("seq #{}", p.0),
            AnyId::Track(t) => format!("track #{}", t.0),
            AnyId::Buffer(b) => format!("buffer #{}", b.0),
            AnyId::Fx { id, .. } => format!("effect #{id}"),
            AnyId::Wave { id, .. } => format!("wave #{id}"),
        })
    }

    fn take_all_freed(&mut self) -> Vec<AnyId> {
        let ids = &mut self.ids;
        let mut freed: Vec<AnyId> = Vec::new();
        freed.extend(ids.voices.take_freed().into_iter().map(AnyId::Voice));
        freed.extend(ids.groups.take_freed().into_iter().map(AnyId::Group));
        freed.extend(ids.tempos.take_freed().into_iter().map(AnyId::Tempo));
        freed.extend(ids.procs.take_freed().into_iter().map(AnyId::Proc));
        freed.extend(ids.tracks.take_freed().into_iter().map(AnyId::Track));
        freed.extend(ids.buffers.take_freed().into_iter().map(AnyId::Buffer));
        freed.extend(
            std::mem::take(&mut self.freed_fx)
                .into_iter()
                .map(|(owner, id)| AnyId::Fx { owner, id }),
        );
        freed.extend(
            std::mem::take(&mut self.freed_waves)
                .into_iter()
                .map(|(voice, id)| AnyId::Wave { voice, id }),
        );
        // a removal that waited for something that is gone has nothing to do
        self.limbo.retain(|l| !freed.contains(&l.id));
        freed
    }

    /// The warnings a statement raised: the pipes it emptied (W102), and a
    /// W103 for each waiting line that named something it removed. The
    /// engine drops those lines as it applies the removal, so the mirror
    /// forgets them here.
    pub(crate) fn settle_warnings(&mut self) -> Vec<(Code, String)> {
        let mut warns = std::mem::take(&mut self.warnings);
        // undoing what a dropped line did frees more IDs, which may drop
        // more lines
        loop {
            let more = self.settle_freed(&mut warns);
            if !more {
                break;
            }
        }
        warns
    }

    // one round of `settle_warnings`; true if undoing made more IDs free
    fn settle_freed(&mut self, warns: &mut Vec<(Code, String)>) -> bool {
        let freed = self.take_all_freed();
        if freed.is_empty() || self.waiting.is_empty() {
            return false;
        }
        let (dropped, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|w| w.ids.iter().any(|(id, _)| freed.contains(id)));
        // what a dropped entry nested never arrives: forget those too, and
        // say nothing more about them
        let mut gone: Vec<u64> = dropped.iter().map(|w| w.seq_no).collect();
        let mut kept = kept;
        let mut nested = Vec::new();
        while let Some(i) = kept
            .iter()
            .position(|w| w.parent.is_some_and(|p| gone.contains(&p)))
        {
            let line = kept.remove(i);
            gone.push(line.seq_no);
            nested.push(line);
        }
        self.waiting = kept;
        let (roots, inner): (Vec<_>, Vec<_>) = dropped
            .into_iter()
            .partition(|w| w.parent.is_none_or(|p| !gone.contains(&p)));
        let mut again = false;
        for line in roots {
            let mut names: Vec<&str> = Vec::new();
            for (_, name) in line.ids.iter().filter(|(id, _)| freed.contains(id)) {
                if !names.contains(&name.as_str()) {
                    names.push(name);
                }
            }
            let mut said = format!(
                "the waiting line '{}' is dropped (it names {})",
                line.text,
                names.join(", ")
            );
            let mut effects = Vec::new();
            for s in line.staged.into_iter().rev() {
                effects.extend(self.undo(s));
            }
            if !effects.is_empty() {
                said.push_str(&format!("; {}", effects.join(", ")));
                again = true;
            }
            warns.push((Code::WaitingLineDropped, said));
        }
        // lines nested in a dropped one never arrive: what they made and
        // removed is undone too, silently
        for line in inner.into_iter().chain(nested) {
            for s in line.staged.into_iter().rev() {
                again |= !self.undo(s).is_empty();
            }
        }
        again
    }

    /// Output channels of the device.
    pub fn out_channels(&self) -> usize {
        self.out_channels
    }

    /// Where the tracks were loaded from, if known.
    pub fn assets_dir(&self) -> Option<&Path> {
        self.assets_dir.as_deref()
    }

    /// Records where the tracks were loaded from.
    pub fn set_assets_dir(&mut self, dir: Option<PathBuf>) {
        self.assets_dir = dir;
    }

    /// `kick` or `g.kick`.
    pub fn display_name(&self, voice: VoiceId) -> Option<String> {
        let v = self.voices.get(&voice)?;
        match v.group.and_then(|g| self.groups.get(&g)) {
            Some(g) => Some(format!("{}.{}", g.name, v.name)),
            None => Some(v.name.clone()),
        }
    }

    pub(crate) fn track(&self, name: &str) -> CmdResult<&TrackRepr> {
        self.tracks.get(name).ok_or_else(|| no_item("track", name))
    }

    // The lookups by name below are the tests' (the language has `lang::names`).

    /// The Voice named "voice" or "group.voice".
    #[cfg(test)]
    pub(crate) fn voice_id(&self, name: &str) -> CmdResult<VoiceId> {
        match split_voice(name)? {
            (None, voice) => self
                .voice_names
                .get(voice)
                .copied()
                .ok_or_else(|| no_voice(voice, None)),
            (Some(group), voice) => {
                let g = self.group_id(group)?;
                self.groups
                    .get(&g)
                    .and_then(|g| g.member_names.get(voice))
                    .copied()
                    .ok_or_else(|| no_voice(voice, Some(group)))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn group_id(&self, name: &str) -> CmdResult<GroupId> {
        self.group_names
            .get(name)
            .copied()
            .ok_or_else(|| no_item("Group", name))
    }

    /// The session's `main` clock, once `CmdProcessor::boot` has made it.
    pub(crate) fn main_id(&self) -> Option<TempoId> {
        self.context_names.get(MAIN).copied()
    }

    /// A TempoContext's tempo.
    #[cfg(test)]
    pub(crate) fn context_id(&self, name: &str) -> CmdResult<TempoId> {
        self.context_names
            .get(name)
            .copied()
            .ok_or_else(|| no_item("TempoContext", name))
    }

    /// Every object of `kind`, in ID order: every voice (top-level and
    /// grouped), every group, or every tempo context (REQ-006).
    pub fn all_ids(&self, kind: IdKind) -> Vec<Target> {
        match kind {
            IdKind::Voice => self
                .voices
                .values()
                .filter(|v| self.sees_voice(v))
                .map(|v| Target::Voice(v.id))
                .collect(),
            IdKind::Group => self
                .groups
                .keys()
                .filter(|g| !self.hides_group(**g))
                .map(|&g| Target::Group(g))
                .collect(),
            IdKind::Tempo => {
                let mut ids: Vec<TempoId> = self.context_names.values().copied().collect();
                ids.sort_unstable();
                ids.into_iter().map(Target::Tempo).collect()
            }
        }
    }

    /// Removes a voice (top-level or grouped) with its processes and owned
    /// tempos from the mirror and returns the Command that does the same in
    /// the engine (REQ-039 item 4).
    pub(crate) fn unload_voice(&mut self, voice: VoiceId) -> CmdResult<Command> {
        let Some(removed) = self.voices.remove(&voice) else {
            return Err(no_item("Voice", &voice.0.to_string()));
        };

        self.limbo.retain(|l| l.id != AnyId::Voice(voice));
        match removed.group.and_then(|g| self.groups.get_mut(&g)) {
            Some(group) => {
                if group.member_names.get(&removed.name) == Some(&voice) {
                    group.member_names.remove(&removed.name);
                }
                group.members.retain(|m| *m != voice);
            }
            None => {
                // the name may be a new voice's by now
                if self.voice_names.get(&removed.name) == Some(&voice) {
                    self.voice_names.remove(&removed.name);
                }
            }
        }

        for proc in removed.procs.values() {
            self.procs.remove(proc);
            self.ids.procs.free(*proc);
        }

        // its own tempo and its processes' own tempos
        let owned: Vec<TempoId> = self
            .tempos
            .values()
            .filter(|t| t.owner == TempoOwner::Voice(voice))
            .map(|t| t.id)
            .collect();
        for tempo in owned {
            self.tempos.remove(&tempo);
            self.ids.tempos.free(tempo);
        }

        self.ids.voices.free(voice);
        self.drop_packets_of(ProcOwner::Voice(voice));
        // a pipe that named the voice stops naming it
        let mut cmds = self.strip_receivers(ProcOwner::Voice(voice), &removed.name);
        // a buffer that recorded it records the mix instead
        cmds.extend(self.orphan_buffers(BufSource::Voice(voice), &removed.name));
        if cmds.is_empty() {
            return Ok(Command::Unload(UnloadArgs { voice }));
        }
        cmds.push(Command::Unload(UnloadArgs { voice }));
        Ok(Command::Batch(BatchArgs { cmds }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_pool_allocates_up_to_limit() {
        let mut pool = IdPool::<VoiceId>::new(3);
        assert_eq!(pool.alloc(), Some(VoiceId(0)));
        assert_eq!(pool.alloc(), Some(VoiceId(1)));
        assert_eq!(pool.alloc(), Some(VoiceId(2)));
        assert_eq!(pool.alloc(), None);
        assert_eq!(pool.limit(), 3);
    }

    #[test]
    fn id_pool_reuses_freed_lifo() {
        let mut pool = IdPool::<GroupId>::new(4);
        let ids: Vec<GroupId> = (0..4).filter_map(|_| pool.alloc()).collect();
        pool.free(ids[1]);
        pool.free(ids[3]);
        assert_eq!(pool.alloc(), Some(GroupId(3)));
        assert_eq!(pool.alloc(), Some(GroupId(1)));
        assert_eq!(pool.alloc(), None);
    }

    use crate::audio_processing::commands::lang::Code;
    use crate::audio_processing::commands::testing::{lerr, loaded, ok, processor};

    #[test]
    fn all_ids_in_id_order() {
        let mut cp = loaded(&["a", "b", "c", "d"]);
        ok(&mut cp, "remove a");
        ok(&mut cp, "group g2 on 240b { d }");
        ok(&mut cp, "group g1 on 240b { b }");
        ok(&mut cp, "clock y 1b");
        ok(&mut cp, "clock x 1b");
        let es = &cp.engine_state;
        let ids = |kind| es.all_ids(kind);

        // c is top-level; b and d are grouped
        assert_eq!(
            ids(IdKind::Voice),
            [1, 2, 3].map(|v| Target::Voice(VoiceId(v)))
        );
        assert_eq!(
            ids(IdKind::Group),
            [0, 1].map(|g| Target::Group(GroupId(g)))
        );
        let (x, y) = (es.context_id("x").unwrap(), es.context_id("y").unwrap());
        assert!(y < x);
        assert_eq!(ids(IdKind::Tempo), vec![Target::Tempo(y), Target::Tempo(x)]);
        assert!(
            processor(&[])
                .engine_state
                .all_ids(IdKind::Voice)
                .is_empty()
        );
    }

    #[test]
    fn all_is_a_reserved_name() {
        let mut cp = loaded(&["a"]);
        for line in [
            "voice all from \"a\" on 1000ms",
            "group all on 240b { a }",
            "clock all 1b",
            "a { seq all 1 }",
        ] {
            let errs = lerr(&mut cp, line);
            assert!(
                errs.iter().any(|e| e.code == Code::ReservedWord),
                "{line}: {}",
                errs[0]
            );
        }
        assert_eq!(cp.engine_state.ids.groups.live(), 0);
        assert!(cp.engine_state.procs.is_empty());

        // a track called `all` imports as a track, but no voice takes the name
        let mut cp = processor(&["all"]);
        assert!(cp.engine_state.track("all").is_ok());
        let errs = lerr(&mut cp, "voice all from \"all\" on 1000ms");
        assert!(errs.iter().any(|e| e.code == Code::ReservedWord));
    }

    #[test]
    fn id_pool_live_count() {
        let mut pool = IdPool::<TrackId>::new(8);
        assert_eq!(pool.live(), 0);
        let a = pool.alloc().unwrap();
        let _b = pool.alloc().unwrap();
        assert_eq!(pool.live(), 2);
        pool.free(a);
        assert_eq!(pool.live(), 1);
        pool.alloc();
        assert_eq!(pool.live(), 2);
        assert_eq!(TrackId(5).index(), 5);
    }
}
