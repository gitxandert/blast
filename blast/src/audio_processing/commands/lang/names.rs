//! The session's one namespace (G1): clocks, voices (grouped or not) and
//! groups share it, across kinds (D3).

use super::ast::{Ident, Kind};
use super::errors::{Code, LangErr};
use crate::audio_processing::commands::reprs::{
    BufferId, EngineState, GroupId, PacketId, ProcId, ProcOwner, TempoId, VoiceId,
};
use crate::audio_processing::commands::{AnyId, FxOwner};
use crate::audio_processing::engine::FxKind;
use crate::audio_processing::engine::fx::spec;

/// The source word for the whole mix (`buffer b from=out`).
pub const OUT: &str = "out";

/// What a name stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Clock(TempoId),
    Voice(VoiceId),
    Group(GroupId),
    /// A buffer (RECb).
    Buffer(BufferId),
    /// A process of a voice or a group (`owner.name`).
    Seq(ProcId),
    /// An lfo of a voice or a group (`owner.name`).
    Lfo(ProcId),
    /// An env of a voice or a group (`owner.name`).
    Env(ProcId),
    /// A packet (`owner.name`, or a top-level name).
    Packet(PacketId),
    /// An effect of a voice (`voice.name`); `kind` is the language kind its
    /// type belongs to (`filter`, `drive`, …).
    Fx {
        owner: FxOwner,
        id: u16,
        kind: Kind,
    },
    /// A wave of a voice (`voice.name`), by its id within the voice.
    Wave {
        voice: VoiceId,
        id: u8,
    },
}

/// The language kind of an effect type: the four filter types are `filter`.
pub fn fx_language_kind(kind: FxKind) -> Kind {
    Kind::Fx(kind.ty)
}

impl Named {
    /// The kind of object this is.
    pub fn kind(self) -> Kind {
        match self {
            Named::Clock(_) => Kind::Clock,
            Named::Voice(_) => Kind::Voice,
            Named::Group(_) => Kind::Group,
            Named::Buffer(_) => Kind::Buffer,
            Named::Seq(_) => Kind::Seq,
            Named::Lfo(_) => Kind::Lfo,
            Named::Env(_) => Kind::Env,
            Named::Packet(_) => Kind::Packet,
            Named::Fx { kind, .. } => kind,
            Named::Wave { .. } => Kind::Wave,
        }
    }
}

/// The word for what `kind` is, for messages: "a voice".
pub fn article(kind: Kind) -> &'static str {
    match kind {
        Kind::Clock => "a clock",
        Kind::Voice => "a voice",
        Kind::Group => "a group",
        Kind::Track => "a track",
        Kind::Seq => "a seq",
        Kind::Packet => "a packet",
        Kind::Lfo => "an lfo",
        Kind::Env => "an env",
        Kind::Fx(t) => spec(t).article,
        Kind::Wave => "a wave",
        Kind::Buffer => "a buffer",
    }
}

// a process by what it is: a seq, an lfo or an env
fn proc_named(es: &EngineState, p: ProcId) -> Named {
    match es.procs.get(&p) {
        Some(r) if r.lfo.is_some() => Named::Lfo(p),
        Some(r) if r.env.is_some() => Named::Env(p),
        _ => Named::Seq(p),
    }
}

// A line typed now sees a voice whose removal waits under its old name,
// before any other; a line that waits sees it gone (PKb3).
fn voice_named(es: &EngineState, name: &str) -> Option<VoiceId> {
    if !es.staging
        && let Some(v) = es.limbo_voice(name, None)
    {
        return Some(v);
    }
    es.voice_names.get(name).copied().or_else(|| {
        // grouped voices keep their names (G1)
        es.groups
            .iter()
            .filter(|(g, _)| !es.hides_group(**g))
            .find_map(|(_, g)| g.member_names.get(name).copied())
    })
}

fn group_named(es: &EngineState, name: &str) -> Option<GroupId> {
    if !es.staging
        && let Some(g) = es.limbo_group(name)
    {
        return Some(g);
    }
    es.group_names.get(name).copied()
}

fn member_named(es: &EngineState, group: GroupId, name: &str) -> Option<VoiceId> {
    if !es.staging
        && let Some(v) = es.limbo_voice(name, Some(Some(group)))
    {
        return Some(v);
    }
    es.groups.get(&group)?.member_names.get(name).copied()
}

fn clock_named(es: &EngineState, name: &str) -> Option<TempoId> {
    es.clock_named(name)
}

fn packet_named(es: &EngineState, owner: ProcOwner, name: &str) -> Option<PacketId> {
    es.packet_of(owner, name).filter(|pk| !es.hides_packet(*pk))
}

/// Every name in the session: clocks, voices (grouped or not) and groups.
pub fn all_names(es: &EngineState) -> Vec<&str> {
    let mut names: Vec<&str> = es.context_names.keys().map(String::as_str).collect();
    names.extend(es.voices.values().map(|v| v.name.as_str()));
    names.extend(es.group_names.keys().map(String::as_str));
    names.extend(es.buffers.keys().map(String::as_str));
    names.extend(es.session_procs.keys().map(String::as_str));
    names.extend(
        es.packets
            .values()
            .filter(|p| p.owner == ProcOwner::Session)
            .map(|p| p.name.as_str()),
    );
    names.sort_unstable();
    names
}

/// What `path` names: one segment searches clocks, voices and groups; two
/// segments are `group.voice` (which must agree with the voice's group,
/// E304), `group.process` or `voice.process`; three are `group.voice.process`.
pub fn lookup(es: &EngineState, path: &[Ident]) -> Result<Named, LangErr> {
    let named = lookup_any(es, path)?;
    // a line that waits may name what another waiting line makes; one typed
    // now may not
    if !es.staging && es.is_pending(any_id(named)) {
        let span = path
            .first()
            .zip(path.last())
            .map(|(a, b)| a.span.to(b.span))
            .unwrap_or_default();
        let text: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
        return Err(LangErr::new(
            Code::NotCreatedYet,
            span,
            format!(
                "'{}' doesn't exist yet (a waiting line creates it)",
                text.join(".")
            ),
        )
        .with_hint("name it in a line that waits too, or wait for it"));
    }
    Ok(named)
}

/// The ID a name stands for.
pub fn any_id(named: Named) -> AnyId {
    match named {
        Named::Clock(t) => AnyId::Tempo(t),
        Named::Voice(v) => AnyId::Voice(v),
        Named::Group(g) => AnyId::Group(g),
        Named::Buffer(b) => AnyId::Buffer(b),
        Named::Seq(p) | Named::Lfo(p) | Named::Env(p) => AnyId::Proc(p),
        // packets live in the mirror only
        Named::Packet(_) => AnyId::Tempo(TempoId(u32::MAX)),
        Named::Fx { owner, id, .. } => AnyId::Fx { owner, id },
        Named::Wave { voice, id } => AnyId::Wave { voice, id },
    }
}

fn lookup_any(es: &EngineState, path: &[Ident]) -> Result<Named, LangErr> {
    match path {
        [one] => {
            let name = one.name.as_str();
            if let Some(t) = clock_named(es, name) {
                Ok(Named::Clock(t))
            } else if let Some(v) = voice_named(es, name) {
                Ok(Named::Voice(v))
            } else if let Some(g) = group_named(es, name) {
                Ok(Named::Group(g))
            } else if let Some(b) = es.buffers.get(name) {
                Ok(Named::Buffer(b.id))
            } else if let Some(p) = es.proc_named(ProcOwner::Session, name) {
                Ok(Named::Seq(p))
            } else if let Some(p) = packet_named(es, ProcOwner::Session, name) {
                Ok(Named::Packet(p))
            } else {
                Err(unknown(es, one))
            }
        }
        [first, second] => {
            let Some(g) = group_named(es, &first.name) else {
                if clock_named(es, &first.name).is_some() {
                    return Err(LangErr::new(
                        Code::AmbiguousPath,
                        first.span,
                        format!("'{}' is a clock, not a group", first.name),
                    ));
                }
                if let Some(v) = voice_named(es, &first.name) {
                    // `voice.name` is a process path, unless the second part
                    // is a voice
                    if voice_named(es, &second.name).is_some() {
                        return Err(LangErr::new(
                            Code::AmbiguousPath,
                            first.span,
                            format!("'{}' is a voice, not a group", first.name),
                        ));
                    }
                    return process_of(es, v, first, second);
                }
                let groups = es.group_names.keys().map(String::as_str);
                return Err(LangErr::new(
                    Code::UnknownName,
                    first.span,
                    format!("unknown group '{}'", first.name),
                )
                .with_suggestion(&first.name, groups));
            };
            let members = &es.groups[&g].member_names;
            if let Some(v) = member_named(es, g, &second.name) {
                return Ok(Named::Voice(v));
            }
            // `group.name` is a seq of the group, if it has one by that name
            if let Some(p) = es.proc_named(ProcOwner::Group(g), &second.name) {
                return Ok(proc_named(es, p));
            }
            if let Some(p) = packet_named(es, ProcOwner::Group(g), &second.name) {
                return Ok(Named::Packet(p));
            }
            // or an effect on the group's mix
            if let Some(f) = es.groups[&g].fx.iter().find(|f| f.name == second.name) {
                return Ok(Named::Fx {
                    owner: FxOwner::Group(g),
                    id: f.id,
                    kind: fx_language_kind(f.kind),
                });
            }
            if voice_named(es, &second.name).is_some() {
                return Err(LangErr::new(
                    Code::AmbiguousPath,
                    second.span,
                    format!("'{}' is not in group '{}'", second.name, first.name),
                ));
            }
            let known = members
                .keys()
                .chain(es.groups[&g].procs.keys())
                .chain(es.groups[&g].fx.iter().map(|f| &f.name));
            Err(LangErr::new(
                Code::UnknownName,
                second.span,
                format!("unknown name '{}' in group '{}'", second.name, first.name),
            )
            .with_suggestion(&second.name, known.map(String::as_str)))
        }
        [group, voice, process] => match lookup_any(es, &[group.clone(), voice.clone()])? {
            Named::Voice(v) => process_of(es, v, voice, process),
            _ => Err(LangErr::new(
                Code::AmbiguousPath,
                voice.span,
                format!("'{}.{}' is not a voice", group.name, voice.name),
            )),
        },
        _ => {
            let span = path
                .first()
                .zip(path.last())
                .map(|(a, b)| a.span.to(b.span))
                .unwrap_or_default();
            Err(LangErr::new(
                Code::WrongType,
                span,
                "a path is at most group.voice.name",
            ))
        }
    }
}

// the process or effect `name` of voice `v`
fn process_of(es: &EngineState, v: VoiceId, owner: &Ident, name: &Ident) -> Result<Named, LangErr> {
    let repr = &es.voices[&v];
    let procs = &repr.procs;
    es.proc_named(ProcOwner::Voice(v), &name.name)
        .map(|p| proc_named(es, p))
        .or_else(|| packet_named(es, ProcOwner::Voice(v), &name.name).map(Named::Packet))
        .or_else(|| {
            repr.fx
                .iter()
                .find(|f| f.name == name.name)
                .map(|f| Named::Fx {
                    owner: FxOwner::Voice(v),
                    id: f.id,
                    kind: fx_language_kind(f.kind),
                })
        })
        .or_else(|| {
            repr.waves
                .iter()
                .find(|w| w.name == name.name)
                .map(|w| Named::Wave { voice: v, id: w.id })
        })
        .ok_or_else(|| {
            let known = procs
                .keys()
                .map(String::as_str)
                .chain(repr.fx.iter().map(|f| f.name.as_str()))
                .chain(repr.waves.iter().map(|w| w.name.as_str()));
            LangErr::new(
                Code::UnknownName,
                name.span,
                format!("'{}' has no process '{}'", owner.name, name.name),
            )
            .with_suggestion(&name.name, known)
        })
}

fn unknown(es: &EngineState, id: &Ident) -> LangErr {
    LangErr::new(
        Code::UnknownName,
        id.span,
        format!("unknown name '{}'", id.name),
    )
    .with_suggestion(&id.name, all_names(es))
}

/// A name for a new object: not `all`, and not taken by any kind (D3).
pub fn check_new(es: &EngineState, id: &Ident) -> Result<(), LangErr> {
    if id.name == OUT {
        return Err(LangErr::new(
            Code::ReservedWord,
            id.span,
            format!(
                "'{}' is reserved: it is the whole mix (buffer from=out)",
                id.name
            ),
        ));
    }
    if id.name == crate::audio_processing::commands::RESERVED_ALL {
        return Err(LangErr::new(
            Code::ReservedWord,
            id.span,
            format!("'{}' is reserved", id.name),
        ));
    }
    match lookup(es, std::slice::from_ref(id)) {
        Ok(n) => Err(LangErr::new(
            Code::KindClash,
            id.span,
            format!("'{}' is already {}", id.name, article(n.kind())),
        )),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::commands::lang::errors::Span;

    fn id(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: Span::default(),
        }
    }

    #[test]
    fn unknown_name_suggests() {
        let mut es = EngineState::new(Vec::new(), 2);
        es.context_names.insert("main".into(), TempoId(0));
        let err = lookup(&es, &[id("mian")]).unwrap_err();
        assert_eq!(err.code, Code::UnknownName);
        assert_eq!(err.suggestion.as_deref(), Some("did you mean 'main'?"));
    }
}
