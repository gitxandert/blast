//! Effect statements (FXa): `filter|drive|crush NAME …` in a voice's block or
//! as `voice.NAME …`, a restated `voice.NAME setting=…`, and the
//! start/stop/pause/resume of one. `remove` is in `remove_target`.

use super::*;
use crate::audio_processing::commands::{Bypass, FxOwner};
use crate::audio_processing::engine::FxKind;
use crate::audio_processing::engine::fx::{defaults, param_of, spec};

/// One setting of an effect statement, ready for the engine.
struct Given {
    param: usize,
    value: f32,
    glide: Glide,
}

impl CmdProcessor {
    // a glide in frames by the clock the owner is on right now
    fn fx_glide_frames(&self, owner: FxOwner, g: Glide, at: Span) -> Res<u32> {
        match owner {
            FxOwner::Voice(v) => self.voice_glide_frames(v, g, at),
            FxOwner::Group(gr) => self.group_glide_frames(gr, g, at),
        }
    }

    /// A `filter`, `drive` or `crush` statement, or a restated effect
    /// (`kick.lo cutoff=2000 over 4`). `block_owner` is the voice or group whose block
    /// it is in; outside a block the path is `voice.NAME`. A restated effect
    /// is edited in place and says so itself (W101) when nothing changed.
    /// True if it changed something.
    pub(super) fn lower_fx_stmt(
        &mut self,
        o: &Object,
        block_owner: Option<FxOwner>,
        out: &mut Out,
    ) -> Res<bool> {
        let Target::Path(path) = &o.target else {
            return Err(wrong_type(o.span, "an effect is named by a word, not a track").into());
        };
        let Some((name, owner_path)) = path.split_last() else {
            return Err(wrong_type(o.span, "a statement needs a name").into());
        };
        let at = path_span(path);
        let voice = match block_owner {
            Some(v) if owner_path.is_empty() => v,
            Some(_) => {
                return Err(wrong_type(at, "in a block an effect is named by one word").into());
            }
            None if owner_path.is_empty() => {
                return Err(wrong_type(
                    at,
                    "an effect belongs to a voice or group: write it in its block, or as owner.NAME",
                )
                .into());
            }
            None => match lookup(&self.engine_state, owner_path)? {
                Named::Voice(v) => FxOwner::Voice(v),
                Named::Group(g) => FxOwner::Group(g),
                other => {
                    return Err(wrong_type(
                        at,
                        format!("{} can't hold an effect", article(other.kind())),
                    )
                    .into());
                }
            },
        };

        let existing = self
            .engine_state
            .fx_list(voice)
            .iter()
            .find(|f| f.name == name.name)
            .cloned();
        let kind = match (o.kind, &existing) {
            (Some((k, _)), None) => k,
            (None, Some(f)) => fx_language_kind(f.kind),
            (Some((k, span)), Some(f)) => {
                let have = fx_language_kind(f.kind);
                if k != have {
                    return Err(LangErr::new(
                        Code::InUse,
                        span,
                        format!("'{}' is {}, not {}", name.name, article(have), article(k)),
                    )
                    .into());
                }
                k
            }
            (None, None) => {
                return Err(LangErr::new(
                    Code::UnknownName,
                    name.span,
                    format!("unknown name '{}'", name.name),
                )
                .into());
            }
        };

        let creating = existing.is_none();
        if creating {
            self.check_new_fx(voice, name)?;
        }
        if o.block.is_some() {
            return Err(wrong_type(o.span, format!("{} takes no block", kind_word(kind))).into());
        }
        if let Some(receivers) = &o.pipe {
            let span = receivers.first().map_or(o.span, |r| r.kind.1);
            return Err(wrong_type(span, format!("{} takes no pipe", kind_word(kind))).into());
        }
        let Kind::Fx(fx_ty) = kind else {
            return Err(wrong_type(o.span, "not an effect").into());
        };
        let sc = schema(kind);
        let checked = check_items(sc, &name.name, &o.items, creating, o.span)?;

        // the type, and every other setting as a parameter
        let mut ty = None;
        let mut given: Vec<Given> = Vec::new();
        for setting in sc.settings {
            let Some(s) = checked.setting(setting.name) else {
                continue;
            };
            if setting.name == "type" {
                if let Value::Word(w) = s.value {
                    ty = spec(fx_ty).types.iter().position(|t| *t == w.name);
                }
                continue;
            }
            let Some(param) = param_of(fx_ty, setting.name) else {
                continue;
            };
            let value = match s.value {
                Value::Num(n) => eval_num(n)?,
                Value::Db(n) => db_to_linear(eval_num(n)?),
                _ => {
                    return Err(
                        wrong_type(s.span, format!("{} takes a number", setting.name)).into(),
                    );
                }
            };
            given.push(Given {
                param,
                value: value as f32,
                glide: Glide::of(s.over)?,
            });
        }

        let Some(repr) = existing else {
            let fx_kind = FxKind {
                ty: fx_ty,
                variant: ty.unwrap_or(0) as u8,
            };
            // a glide on a new effect starts from the default
            let mut params = defaults(fx_ty);
            for g in given.iter().filter(|g| g.glide == Glide::Instant) {
                params[g.param] = g.value;
            }
            let cmd = self
                .make_fx(voice, &name.name, fx_kind, params)
                .map_err(|e| self.cmd_err(e, o.span))?;
            out.push(cmd);
            let id = self.engine_state.fx_list(voice).last().map_or(0, |f| f.id);
            for g in given.iter().filter(|g| g.glide != Glide::Instant) {
                if g.value != params[g.param] {
                    let frames = self.fx_glide_frames(voice, g.glide, o.span)?;
                    let cmd = self.set_fx_param(voice, id, g.param, g.value, frames);
                    out.push(cmd);
                }
            }
            return Ok(true);
        };

        let mut changed = false;
        if let Some(variant) = ty
            && variant as u8 != repr.kind.variant
        {
            let new_kind = FxKind {
                ty: fx_ty,
                variant: variant as u8,
            };
            let cmd = self.retype_fx(voice, repr.id, new_kind);
            out.push(cmd);
            changed = true;
        }
        for g in &given {
            if g.value != repr.params[g.param] {
                let frames = self.fx_glide_frames(voice, g.glide, o.span)?;
                let cmd = self.set_fx_param(voice, repr.id, g.param, g.value, frames);
                out.push(cmd);
                changed = true;
            }
        }
        if !changed {
            out.warn_unchanged(o.span, &format!("'{}'", name.name));
        }
        Ok(changed)
    }

    // a name for a new effect: not `all`, and not a seq or packet of the
    // same voice (they share its scope)
    fn check_new_fx(&self, owner: FxOwner, name: &Ident) -> Res<()> {
        if name.name == RESERVED_ALL {
            return Err(LangErr::new(
                Code::ReservedWord,
                name.span,
                format!("'{}' is reserved", name.name),
            )
            .into());
        }
        let es = &self.engine_state;
        let (procs, proc_owner) = match owner {
            FxOwner::Voice(v) => (&es.voices[&v].procs, ProcOwner::Voice(v)),
            FxOwner::Group(g) => (&es.groups[&g].procs, ProcOwner::Group(g)),
        };
        let member = matches!(owner, FxOwner::Group(g)
            if es.groups[&g].member_names.contains_key(&name.name));
        let clash = if procs.contains_key(&name.name) {
            Some("a seq")
        } else if es.packet_of(proc_owner, &name.name).is_some() {
            Some("a packet")
        } else if matches!(owner, FxOwner::Voice(v) if es.has_wave(v, &name.name)) {
            Some("a wave")
        } else if member {
            Some("a voice")
        } else {
            None
        };
        match clash {
            Some(what) => Err(LangErr::new(
                Code::KindClash,
                name.span,
                format!("'{}' is already {what}", name.name),
            )
            .into()),
            None => Ok(()),
        }
    }

    /// `start`, `stop`, `pause` or `resume` of one effect. `stop` bypasses it
    /// and clears its memory, `pause` bypasses it and keeps the memory;
    /// `start` and `resume` bring it back (`start` from a clean state). One
    /// that is already in that state warns and does nothing.
    pub(super) fn fx_action(
        &mut self,
        word: ActionWord,
        voice: FxOwner,
        id: u16,
        at: Span,
        out: &mut Out,
    ) -> Option<Command> {
        let repr = self
            .engine_state
            .fx_list(voice)
            .iter()
            .find(|f| f.id == id)?;
        let (name, now) = (repr.name.clone(), repr.bypass);
        let (to, clear) = match word {
            ActionWord::Stop if now != Some(Bypass::Stopped) => (Some(Bypass::Stopped), true),
            ActionWord::Pause if now.is_none() => (Some(Bypass::Paused), false),
            ActionWord::Start if now.is_some() => (None, true),
            ActionWord::Resume if now.is_some() => (None, false),
            _ => {
                out.warn_unchanged(at, &format!("'{name}'"));
                return None;
            }
        };
        Some(self.bypass_fx(voice, id, to, clear))
    }
}
