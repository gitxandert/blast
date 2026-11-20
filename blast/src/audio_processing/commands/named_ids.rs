//! Which objects a command names (W5FIX1). One exhaustive walk over
//! `Command`, with no wildcard arms, so a new command or field has to say
//! what it names. The command thread uses it to record what a waiting line
//! names; the engine uses it to drop a waiting line whose object was
//! removed. It allocates nothing.

use crate::audio_processing::engine::{BufSource, MAX_WAVES};

use super::{
    BufOp, BufferId, Command, FxOp, FxOwner, GlideRef, GroupId, GroupOp, LoadSource, ProcId,
    ProcOp, ProcOwner, RecvSpec, TapOwner, Target, TempoId, TempoTarget, TempoUse, TrackId,
    TrigTarget, VoiceId, WaveOp, WhenSpec,
};

/// An ID of any kind, as a command names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnyId {
    Voice(VoiceId),
    Group(GroupId),
    Tempo(TempoId),
    Proc(ProcId),
    Track(TrackId),
    Buffer(BufferId),
    /// An effect of a voice or group, by its id within the owner.
    Fx {
        owner: FxOwner,
        id: u16,
    },
    /// A wave of a voice, by its id (slot) within the voice.
    Wave {
        voice: VoiceId,
        id: u8,
    },
}

/// Calls `f` with every ID `cmd` names (an ID may come up more than once).
pub fn visit_ids(cmd: &Command, f: &mut impl FnMut(AnyId)) {
    match cmd {
        Command::Load(a) => {
            f(AnyId::Voice(a.voice));
            if let LoadSource::Track(t) = a.source {
                f(AnyId::Track(t));
            }
            tempo_use(&a.tempo, f);
        }
        Command::Start(a) => target(a.target, f),
        Command::Pause(a) => target(a.target, f),
        Command::Resume(a) => target(a.target, f),
        Command::Stop(a) => target(a.target, f),
        Command::Unload(a) => f(AnyId::Voice(a.voice)),
        Command::SetParam(a) => f(AnyId::Voice(a.voice)),
        Command::SetMode(a) => f(AnyId::Voice(a.voice)),
        Command::SetShifters(a) => f(AnyId::Voice(a.voice)),
        Command::SetTrack(a) => {
            f(AnyId::Voice(a.voice));
            f(AnyId::Track(a.track));
        }
        Command::AddTrack(a) => f(AnyId::Track(a.id)),
        Command::DropTrack(a) => f(AnyId::Track(a.id)),
        Command::Group(a) => {
            f(AnyId::Group(a.group));
            tempo_use(&a.tempo, f);
            for m in &a.members {
                f(AnyId::Voice(*m));
            }
        }
        Command::GroupEdit(a) => {
            f(AnyId::Group(a.group));
            match a.op {
                GroupOp::Join(v) | GroupOp::Leave(v) => f(AnyId::Voice(v)),
                GroupOp::Remove => {}
            }
        }
        Command::GroupGain(a) => f(AnyId::Group(a.group)),
        Command::Tc(a) => f(AnyId::Tempo(a.tempo.id)),
        Command::TempoSet(a) => f(AnyId::Tempo(a.id)),
        Command::TempoFree(a) => f(AnyId::Tempo(a.id)),
        Command::TempoAssign(a) => {
            match a.target {
                TempoTarget::Voice(v) => f(AnyId::Voice(v)),
                TempoTarget::Group(g) => f(AnyId::Group(g)),
                TempoTarget::Proc(p) => f(AnyId::Proc(p)),
            }
            tempo_use(&a.tempo, f);
        }
        Command::Seq(a) => {
            f(AnyId::Proc(a.proc));
            owner(a.owner, f);
            tempo_use(&a.tempo, f);
            recv(&a.recv, f);
        }
        Command::SeqEdit(a) => f(AnyId::Proc(a.id)),
        Command::Lfo(a) => {
            f(AnyId::Proc(a.proc));
            owner(a.owner, f);
            tempo_use(&a.tempo, f);
        }
        Command::LfoEdit(a) => f(AnyId::Proc(a.id)),
        Command::Env(a) => {
            f(AnyId::Proc(a.proc));
            owner(a.owner, f);
            tempo_use(&a.tempo, f);
        }
        Command::EnvEdit(a) => f(AnyId::Proc(a.id)),
        Command::SeqRecv(a) => {
            f(AnyId::Proc(a.id));
            recv(&a.recv, f);
        }
        Command::ProcEdit(a) => f(AnyId::Proc(a.id)),
        Command::Fx(a) => {
            match a.owner {
                FxOwner::Voice(v) => f(AnyId::Voice(v)),
                FxOwner::Group(g) => f(AnyId::Group(g)),
            }
            // ids are reused, so a line must not outlive the effect it names
            f(AnyId::Fx {
                owner: a.owner,
                id: a.id,
            });
        }
        Command::Wave(a) => {
            f(AnyId::Voice(a.voice));
            // ids are reused, so a line must not outlive the wave it names
            f(AnyId::Wave {
                voice: a.voice,
                id: a.id,
            });
            // nor the modulator it follows
            if let WaveOp::Fm(mask) = a.op {
                for m in (0..MAX_WAVES as u8).filter(|m| mask >> m & 1 == 1) {
                    f(AnyId::Wave {
                        voice: a.voice,
                        id: m,
                    });
                }
            }
        }
        Command::Buffer(a) => {
            f(AnyId::Buffer(BufferId(a.slot)));
            buf_source(a.source, f);
        }
        Command::BufferEdit(a) => {
            f(AnyId::Buffer(BufferId(a.slot)));
            if let BufOp::Source(s) = a.op {
                buf_source(s, f);
            }
        }
        Command::SetTaps(a) => match a.owner {
            TapOwner::Voice(v) => f(AnyId::Voice(v)),
            TapOwner::Group(g) => f(AnyId::Group(g)),
            TapOwner::Out => {}
        },
        Command::SetSource(a) => f(AnyId::Voice(a.voice)),
        Command::Schedule(a) => {
            match &a.when {
                WhenSpec::At { tempo, .. }
                | WhenSpec::After { tempo, .. }
                | WhenSpec::AfterMark { tempo, .. } => f(AnyId::Tempo(*tempo)),
                // what a `thenif` watches must still be there
                WhenSpec::AfterGlides { watch } => {
                    for w in watch.iter().flat_map(|w| w.iter()) {
                        glide_ids(w.glide, f);
                    }
                }
            }
            visit_ids(&a.cmd, f);
        }
        Command::MarkGlides(_) | Command::CancelScript(_) => {}
        Command::SetMark(a) => f(AnyId::Tempo(a.tempo)),
        Command::Batch(a) => {
            for c in &a.cmds {
                visit_ids(c, f);
            }
        }
        Command::Quit(_) => {}
    }
}

/// The waves a command's `Fm` ops name as modulators (through a waiting
/// line and a batch too): what a waiting `fm=` line will link.
pub fn fm_modulator_ids(cmd: &Command, out: &mut Vec<AnyId>) {
    match cmd {
        Command::Wave(a) => {
            if let WaveOp::Fm(mask) = a.op {
                for m in (0..MAX_WAVES as u8).filter(|m| mask >> m & 1 == 1) {
                    out.push(AnyId::Wave {
                        voice: a.voice,
                        id: m,
                    });
                }
            }
        }
        Command::Schedule(a) => fm_modulator_ids(&a.cmd, out),
        Command::Batch(a) => a.cmds.iter().for_each(|c| fm_modulator_ids(c, out)),
        _ => {}
    }
}

fn glide_ids(g: GlideRef, f: &mut impl FnMut(AnyId)) {
    match g {
        GlideRef::Voice { voice, .. } => f(AnyId::Voice(voice)),
        GlideRef::Group(g) => f(AnyId::Group(g)),
        GlideRef::Tempo(t) => f(AnyId::Tempo(t)),
        GlideRef::Fx { owner, id, .. } => {
            match owner {
                FxOwner::Voice(v) => f(AnyId::Voice(v)),
                FxOwner::Group(g) => f(AnyId::Group(g)),
            }
            f(AnyId::Fx { owner, id });
        }
        GlideRef::Wave { voice, id, .. } => {
            f(AnyId::Voice(voice));
            f(AnyId::Wave { voice, id });
        }
        GlideRef::Proc(p) => f(AnyId::Proc(p)),
    }
}

/// Adds to `out` every glide `cmd` starts (a setting `over` something).
/// `thenif` watches them. Written out for every variant, so a new command
/// has to say whether it glides. Does not look inside a `Schedule`: what
/// waits hasn't started anything.
pub fn glide_refs(cmd: &Command, out: &mut Vec<GlideRef>) {
    let mut add = |g: GlideRef| {
        if !out.contains(&g) {
            out.push(g);
        }
    };
    match cmd {
        Command::SetParam(a) if a.frames > 0 => add(GlideRef::Voice {
            voice: a.voice,
            param: a.param,
        }),
        Command::GroupGain(a) if a.frames > 0 => add(GlideRef::Group(a.group)),
        Command::TempoSet(a) if a.frames > 0 => add(GlideRef::Tempo(a.id)),
        Command::Fx(a) => {
            if let FxOp::Set {
                param, frames: f, ..
            } = a.op
                && f > 0
            {
                add(GlideRef::Fx {
                    owner: a.owner,
                    id: a.id,
                    param,
                });
            }
        }
        Command::Wave(a) => {
            if let WaveOp::Set {
                param, frames: f, ..
            } = a.op
                && f > 0
            {
                add(GlideRef::Wave {
                    voice: a.voice,
                    id: a.id,
                    param,
                });
            }
        }
        Command::LfoEdit(a) => {
            let glides = |e: &Option<super::FeelEdit>| e.is_some_and(|e| e.frames > 0);
            if glides(&a.depth) || glides(&a.offset) || glides(&a.freq) {
                add(GlideRef::Proc(a.id));
            }
        }
        Command::SeqEdit(a) => {
            if a.feel.iter().flatten().any(|e| e.frames > 0) {
                add(GlideRef::Proc(a.id));
            }
        }
        Command::Batch(a) => {
            for c in &a.cmds {
                glide_refs(c, out);
            }
        }
        Command::Load(_)
        | Command::Start(_)
        | Command::Pause(_)
        | Command::Resume(_)
        | Command::Stop(_)
        | Command::Unload(_)
        | Command::SetParam(_)
        | Command::SetMode(_)
        | Command::SetTrack(_)
        | Command::SetShifters(_)
        | Command::AddTrack(_)
        | Command::DropTrack(_)
        | Command::Group(_)
        | Command::GroupEdit(_)
        | Command::GroupGain(_)
        | Command::Tc(_)
        | Command::TempoSet(_)
        | Command::TempoFree(_)
        | Command::TempoAssign(_)
        | Command::Seq(_)
        | Command::SeqRecv(_)
        | Command::Lfo(_)
        | Command::Env(_)
        | Command::EnvEdit(_)
        | Command::ProcEdit(_)
        | Command::Buffer(_)
        | Command::BufferEdit(_)
        | Command::SetTaps(_)
        | Command::SetSource(_)
        | Command::Schedule(_)
        | Command::MarkGlides(_)
        | Command::SetMark(_)
        | Command::CancelScript(_)
        | Command::Quit(_) => {}
    }
}

/// Whether the box of a scheduled entry holds only `MarkGlides`, `SetMark`,
/// `CancelScript` and further `Schedule`s (a head, a tail or a wait of a
/// `then` chain, a script segment): no statement of its own, so a removed
/// object takes the statements nested in it, not it.
pub fn holds_no_statement(cmd: &Command) -> bool {
    match cmd {
        Command::MarkGlides(_)
        | Command::SetMark(_)
        | Command::CancelScript(_)
        | Command::Schedule(_) => true,
        Command::Batch(a) => a.cmds.iter().all(holds_no_statement),
        _ => false,
    }
}

/// Whether `cmd` names `id`.
pub fn names_id(cmd: &Command, id: AnyId) -> bool {
    let mut found = false;
    visit_ids(cmd, &mut |named| found |= named == id);
    found
}

fn buf_source(s: BufSource, f: &mut impl FnMut(AnyId)) {
    match s {
        BufSource::Out => {}
        BufSource::Voice(v) => f(AnyId::Voice(v)),
        BufSource::Group(g) => f(AnyId::Group(g)),
    }
}

fn target(t: Target, f: &mut impl FnMut(AnyId)) {
    match t {
        Target::Voice(v) => f(AnyId::Voice(v)),
        Target::Group(g) => f(AnyId::Group(g)),
        Target::Tempo(t) => f(AnyId::Tempo(t)),
    }
}

fn tempo_use(t: &TempoUse, f: &mut impl FnMut(AnyId)) {
    match t {
        TempoUse::Own(init) => f(AnyId::Tempo(init.id)),
        TempoUse::Shared(id) => f(AnyId::Tempo(*id)),
    }
}

fn owner(o: ProcOwner, f: &mut impl FnMut(AnyId)) {
    match o {
        ProcOwner::Voice(v) => f(AnyId::Voice(v)),
        ProcOwner::Group(g) => f(AnyId::Group(g)),
        ProcOwner::Session => {}
    }
}

fn recv(specs: &[RecvSpec], f: &mut impl FnMut(AnyId)) {
    for spec in specs {
        match spec {
            RecvSpec::Trig(t) => match t {
                TrigTarget::Owner => {}
                TrigTarget::Voice(v) => f(AnyId::Voice(*v)),
                TrigTarget::Group(g) => f(AnyId::Group(*g)),
            },
            RecvSpec::Melo { target, .. } => f(AnyId::Voice(*target)),
        }
    }
}

/// Whether `cmd` makes or frees something, or allocates or frees an ID: such
/// a line can't wait, because the mirror's IDs would run ahead of the
/// engine's (SCHED). The engine also sweeps its waiting lines after one. A `Schedule` does if
/// what waits in it does.
/// Written out for every variant, so a new command has to say which it is.
pub fn creates_or_removes(cmd: &Command) -> bool {
    match cmd {
        Command::Load(_)
        | Command::Unload(_)
        | Command::AddTrack(_)
        | Command::DropTrack(_)
        | Command::Group(_)
        | Command::Tc(_)
        | Command::TempoFree(_)
        | Command::Seq(_)
        | Command::Lfo(_)
        | Command::Env(_)
        // a buffer takes a slot of the engine's table
        | Command::Buffer(_) => true,
        // what waits inside it may create or remove
        Command::Schedule(a) => creates_or_removes(&a.cmd),
        Command::MarkGlides(_) | Command::SetMark(_) | Command::CancelScript(_) => false,
        // a move onto a clock of its own makes one
        Command::TempoAssign(a) => matches!(a.tempo, TempoUse::Own(_)),
        Command::GroupEdit(a) => matches!(a.op, GroupOp::Remove),
        Command::ProcEdit(a) => matches!(a.op, ProcOp::Remove),
        Command::BufferEdit(a) => matches!(a.op, BufOp::Remove),
        // an insert picks the lowest free id and the chain's room; a removal
        // frees an id
        Command::Fx(a) => matches!(a.op, FxOp::Insert { .. } | FxOp::Remove),
        // a wave takes the voice's lowest free slot
        Command::Wave(a) => matches!(a.op, WaveOp::Insert { .. } | WaveOp::Remove),
        Command::Batch(a) => a.cmds.iter().any(creates_or_removes),
        Command::Start(_)
        | Command::Pause(_)
        | Command::Resume(_)
        | Command::Stop(_)
        | Command::SetParam(_)
        | Command::SetMode(_)
        | Command::SetTrack(_)
        | Command::SetShifters(_)
        | Command::GroupGain(_)
        | Command::TempoSet(_)
        | Command::SeqEdit(_)
        | Command::SeqRecv(_)
        | Command::LfoEdit(_)
        | Command::EnvEdit(_)
        | Command::SetTaps(_)
        | Command::SetSource(_)
        | Command::Quit(_) => false,
    }
}

/// Calls `f` with every ID that `cmd` makes, itself or in the entries it
/// holds (a `Load` makes its voice and a clock of its own, a seq its process).
/// An entry that makes an object is the only thing that may name it before it
/// exists (PKb2). Written out for every variant, so a new command has to say
/// whether it makes something. Allocates nothing.
pub fn created_ids(cmd: &Command, f: &mut impl FnMut(AnyId)) {
    let own = |t: &TempoUse, f: &mut dyn FnMut(AnyId)| {
        if let TempoUse::Own(init) = t {
            f(AnyId::Tempo(init.id));
        }
    };
    match cmd {
        Command::Load(a) => {
            f(AnyId::Voice(a.voice));
            own(&a.tempo, f);
        }
        Command::Group(a) => {
            f(AnyId::Group(a.group));
            own(&a.tempo, f);
        }
        Command::Tc(a) => f(AnyId::Tempo(a.tempo.id)),
        Command::TempoAssign(a) => own(&a.tempo, f),
        Command::Seq(a) => {
            f(AnyId::Proc(a.proc));
            own(&a.tempo, f);
        }
        Command::Lfo(a) => {
            f(AnyId::Proc(a.proc));
            own(&a.tempo, f);
        }
        Command::Env(a) => {
            f(AnyId::Proc(a.proc));
            own(&a.tempo, f);
        }
        Command::Fx(a) => {
            if matches!(a.op, FxOp::Insert { .. }) {
                f(AnyId::Fx {
                    owner: a.owner,
                    id: a.id,
                });
            }
        }
        Command::Buffer(a) => f(AnyId::Buffer(BufferId(a.slot))),
        Command::AddTrack(a) => f(AnyId::Track(a.id)),
        Command::Wave(a) => {
            if matches!(a.op, WaveOp::Insert { .. }) {
                f(AnyId::Wave {
                    voice: a.voice,
                    id: a.id,
                });
            }
        }
        Command::Schedule(a) => created_ids(&a.cmd, f),
        Command::Batch(a) => {
            for c in &a.cmds {
                created_ids(c, f);
            }
        }
        Command::Start(_)
        | Command::Pause(_)
        | Command::Resume(_)
        | Command::Stop(_)
        | Command::Unload(_)
        | Command::SetParam(_)
        | Command::SetMode(_)
        | Command::SetTrack(_)
        | Command::SetShifters(_)
        | Command::DropTrack(_)
        | Command::GroupEdit(_)
        | Command::GroupGain(_)
        | Command::TempoSet(_)
        | Command::TempoFree(_)
        | Command::SeqEdit(_)
        | Command::SeqRecv(_)
        | Command::LfoEdit(_)
        | Command::EnvEdit(_)
        | Command::ProcEdit(_)
        | Command::BufferEdit(_)
        | Command::SetTaps(_)
        | Command::SetSource(_)
        | Command::MarkGlides(_)
        | Command::SetMark(_)
        | Command::CancelScript(_)
        | Command::Quit(_) => {}
    }
}

/// Whether `cmd` (or an entry in it) makes `id`.
pub fn creates_id(cmd: &Command, id: AnyId) -> bool {
    let mut found = false;
    created_ids(cmd, &mut |made| found |= made == id);
    found
}

/// Adds to `out` the objects `cmd` itself makes, removes or moves by name, not those
/// in the entries it holds: the key under which the mirror files what a
/// waiting statement did (PKb2). Written out for every variant.
pub fn effect_ids(cmd: &Command, out: &mut Vec<AnyId>) {
    match cmd {
        Command::Load(a) => out.push(AnyId::Voice(a.voice)),
        Command::Unload(a) => out.push(AnyId::Voice(a.voice)),
        Command::Group(a) => out.push(AnyId::Group(a.group)),
        Command::GroupEdit(a) => match a.op {
            GroupOp::Remove => out.push(AnyId::Group(a.group)),
            // a voice moving in or out of a group is filed under the voice
            GroupOp::Join(v) | GroupOp::Leave(v) => out.push(AnyId::Voice(v)),
        },
        Command::Tc(a) => out.push(AnyId::Tempo(a.tempo.id)),
        Command::TempoFree(a) => out.push(AnyId::Tempo(a.id)),
        Command::Seq(a) => out.push(AnyId::Proc(a.proc)),
        Command::Lfo(a) => out.push(AnyId::Proc(a.proc)),
        Command::Env(a) => out.push(AnyId::Proc(a.proc)),
        Command::ProcEdit(a) => {
            if matches!(a.op, ProcOp::Remove) {
                out.push(AnyId::Proc(a.id));
            }
        }
        Command::Fx(a) => {
            if matches!(a.op, FxOp::Insert { .. } | FxOp::Remove) {
                out.push(AnyId::Fx {
                    owner: a.owner,
                    id: a.id,
                });
            }
        }
        Command::Wave(a) => {
            if matches!(a.op, WaveOp::Insert { .. } | WaveOp::Remove) {
                out.push(AnyId::Wave {
                    voice: a.voice,
                    id: a.id,
                });
            }
        }
        Command::Batch(a) => {
            for c in &a.cmds {
                effect_ids(c, out);
            }
        }
        Command::Start(_)
        | Command::Pause(_)
        | Command::Resume(_)
        | Command::Stop(_)
        | Command::SetParam(_)
        | Command::SetMode(_)
        | Command::SetTrack(_)
        | Command::SetShifters(_)
        | Command::AddTrack(_)
        | Command::DropTrack(_)
        | Command::GroupGain(_)
        | Command::TempoSet(_)
        | Command::TempoAssign(_)
        | Command::SeqEdit(_)
        | Command::SeqRecv(_)
        | Command::LfoEdit(_)
        | Command::EnvEdit(_)
        | Command::Buffer(_)
        | Command::BufferEdit(_)
        | Command::SetTaps(_)
        | Command::SetSource(_)
        | Command::Schedule(_)
        | Command::MarkGlides(_)
        | Command::SetMark(_)
        | Command::CancelScript(_)
        | Command::Quit(_) => {}
    }
}

/// Whether `cmd` (or an entry in it) changes something the mirror can't
/// undo if the line is dropped, so it still can't wait (E410): a move onto a
/// clock of its own, an effect removed, a buffer (its memory and table slot),
/// a track.
pub fn cannot_stage(cmd: &Command) -> bool {
    match cmd {
        Command::TempoAssign(a) => matches!(a.tempo, TempoUse::Own(_)),
        Command::Buffer(_) | Command::AddTrack(_) | Command::DropTrack(_) => true,
        Command::Fx(a) => matches!(a.op, FxOp::Remove),
        Command::Wave(a) => matches!(a.op, WaveOp::Remove),
        Command::BufferEdit(a) => matches!(a.op, BufOp::Remove),
        Command::Schedule(a) => cannot_stage(&a.cmd),
        Command::Batch(a) => a.cmds.iter().any(cannot_stage),
        Command::Load(_)
        | Command::Start(_)
        | Command::Pause(_)
        | Command::Resume(_)
        | Command::Stop(_)
        | Command::Unload(_)
        | Command::SetParam(_)
        | Command::SetMode(_)
        | Command::SetTrack(_)
        | Command::SetShifters(_)
        | Command::Group(_)
        | Command::GroupEdit(_)
        | Command::GroupGain(_)
        | Command::Tc(_)
        | Command::TempoSet(_)
        | Command::TempoFree(_)
        | Command::Seq(_)
        | Command::SeqEdit(_)
        | Command::SeqRecv(_)
        | Command::Lfo(_)
        | Command::LfoEdit(_)
        | Command::Env(_)
        | Command::EnvEdit(_)
        | Command::ProcEdit(_)
        | Command::SetTaps(_)
        | Command::SetSource(_)
        | Command::MarkGlides(_)
        | Command::SetMark(_)
        | Command::CancelScript(_)
        | Command::Quit(_) => false,
    }
}
