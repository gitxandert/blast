//! `env` statements (ENV): `env NAME …` in a voice's or group's block, and a
//! restated `owner.NAME setting=…`. The actions (`pause`, `stop`, …) and
//! `remove` are in `lower_action` and `remove_target`, as for a seq. The
//! target rules are the lfo's.

use super::lfo::unit_text;
use super::*;
use crate::audio_processing::commands::procs::env::{EnvChange, EnvText, NewEnv};
use crate::audio_processing::processes::{EnvCurve, EnvShape, LfoTarget};

// the default target
const GAIN: LfoTarget = LfoTarget::Voice(VoiceParam::Gain);

/// A duration as it was written: `200ms`, `0.5`.
fn dur_text(value: &Value) -> Res<String> {
    Ok(match value {
        Value::Num(n) => format!("{}", eval_num(n)?),
        Value::Duration(n, unit) => format!("{}{}", eval_num(n)?, unit_text(*unit)),
        Value::Word(w) => w.name.clone(),
        _ => String::new(),
    })
}

fn curve_of(word: &str) -> EnvCurve {
    match word {
        "lin" => EnvCurve::Lin,
        _ => EnvCurve::Exp,
    }
}

impl CmdProcessor {
    // a length written as a value, in frames of the owner's clock as it is now
    fn env_frames(&self, value: &Value, owner: ProcOwner, at: Span) -> Res<u32> {
        let glide = Glide::of_value(value, at)?;
        let es = &self.engine_state;
        let (unit, interval) = es
            .owner_tempo(owner)
            .and_then(|t| es.tempos.get(&t))
            .map(|t| (t.unit, t.interval))
            .ok_or_else(|| wrong_type(at, "the env's owner isn't known"))?;
        Ok(glide_frames(
            glide,
            self.lfo_owner_period(owner),
            tick_secs(unit, interval),
        ))
    }

    /// An `env` statement, or a restated env (`kick.amp decay=400ms`).
    /// `block_owner` is the voice or group whose block it is in; outside a
    /// block the path is `owner.NAME`. A restated env is edited in place and
    /// says so itself (W101) when nothing changed. True if it changed
    /// something.
    pub(super) fn lower_env_stmt(
        &mut self,
        o: &Object,
        block_owner: Option<ProcOwner>,
        out: &mut Out,
    ) -> Res<bool> {
        let Target::Path(path) = &o.target else {
            return Err(wrong_type(o.span, "an env is named by a word, not a track").into());
        };
        let Some((name, owner_path)) = path.split_last() else {
            return Err(wrong_type(o.span, "a statement needs a name").into());
        };
        let at = path_span(path);
        let owner = match block_owner {
            Some(p) if owner_path.is_empty() => p,
            Some(_) => {
                return Err(wrong_type(at, "in a block an env is named by one word").into());
            }
            None if owner_path.is_empty() => {
                return Err(wrong_type(
                    at,
                    "an env needs an owner: write it in a voice's or group's block, or as owner.NAME",
                )
                .into());
            }
            None => match lookup(&self.engine_state, owner_path)? {
                Named::Voice(v) => ProcOwner::Voice(v),
                Named::Group(g) => ProcOwner::Group(g),
                other => {
                    return Err(wrong_type(
                        at,
                        format!("{} can't hold an env", article(other.kind())),
                    )
                    .into());
                }
            },
        };

        let existing = self.engine_state.proc_named(owner, &name.name);
        if let Some(p) = existing
            && o.kind.is_some()
            && !self.engine_state.staging
            && self.engine_state.in_limbo(AnyId::Proc(p))
        {
            return Err(in_use_waiting(name.span, &name.name));
        }
        let creating = existing.is_none();
        if let Some(p) = existing
            && self.engine_state.procs[&p].env.is_none()
        {
            let what = if self.engine_state.procs[&p].lfo.is_some() {
                "an lfo"
            } else {
                "a seq"
            };
            return Err(LangErr::new(
                Code::KindClash,
                name.span,
                format!("'{}' is {what}, not an env", name.name),
            )
            .into());
        }
        if creating {
            self.check_new_lfo(owner, name)?;
        }
        if o.block.is_some() {
            return Err(wrong_type(o.span, "env takes no block").into());
        }
        if let Some(pipe) = &o.pipe {
            let span = pipe.first().map_or(o.span, |r| r.kind.1);
            return Err(wrong_type(span, "env takes no pipe").into());
        }
        let checked = check_items(schema(Kind::Env), &name.name, &o.items, creating, o.span)?;

        // the settings that were written
        let num = |s: &str| -> Res<Option<f32>> {
            match checked.setting(s).map(|c| c.value) {
                Some(Value::Num(n)) => Ok(Some(eval_num(n)? as f32)),
                // a depth on pitch: semitones (7st) or cents (50c)
                Some(Value::Pitch(n, unit)) => {
                    let st = pitch_semitones(eval_num(n)?, *unit);
                    Ok(st.map(|v| v as f32))
                }
                _ => Ok(None),
            }
        };
        let (sustain, depth) = (num("sustain")?, num("depth")?);
        let curve = match checked.setting("curve").map(|c| c.value) {
            Some(Value::Word(w)) => Some(curve_of(&w.name)),
            _ => None,
        };
        // frames and text of a duration setting
        let dur = |s: &str| -> Res<Option<(u32, String)>> {
            let Some(c) = checked.setting(s) else {
                return Ok(None);
            };
            Ok(Some((
                self.env_frames(c.value, owner, c.span)?,
                dur_text(c.value)?,
            )))
        };
        let (attack, decay, release) = (dur("attack")?, dur("decay")?, dur("release")?);
        // `hold=retrigger` is the word; a duration is frames
        let hold = match checked.setting("hold") {
            Some(c) => Some(match c.value {
                Value::Word(_) => (None, "retrigger".to_string()),
                v => (Some(self.env_frames(v, owner, c.span)?), dur_text(v)?),
            }),
            None => None,
        };
        let target = match checked.setting("to") {
            Some(CheckedSetting {
                value: Value::Word(w),
                span,
                ..
            }) => Some((
                self.lfo_target("env", &w.name, owner, *span)?,
                w.name.clone(),
            )),
            _ => None,
        };
        let now_text = existing.and_then(|id| {
            let procs = &self.engine_state.procs[&id];
            procs.env.as_ref().map(|r| r.target_text.clone())
        });
        check_depth_unit(
            &checked,
            target.as_ref().map(|t| t.1.as_str()),
            now_text.as_deref(),
        )?;

        // what an env being made starts from, or what a restated one is now
        let (now_shape, now_text, now_target, now_depth) = match existing {
            Some(id) => {
                let r = self.engine_state.procs[&id].env.clone().unwrap_or_else(|| {
                    unreachable!("checked above that the process is an env");
                });
                (r.shape, r.text, (r.target, r.target_text), r.depth)
            }
            None => (
                EnvShape {
                    attack: 0,
                    decay: 0,
                    sustain: 1.0,
                    hold: None,
                    release: 0,
                    curve: EnvCurve::Exp,
                },
                EnvText::default(),
                (GAIN, "gain".to_string()),
                1.0,
            ),
        };

        let mut shape = now_shape;
        let mut text = now_text.clone();
        if let Some((f, t)) = attack {
            (shape.attack, text.attack) = (f, t);
        }
        if let Some((f, t)) = decay {
            (shape.decay, text.decay) = (f, t);
        }
        if let Some((f, t)) = release {
            (shape.release, text.release) = (f, t);
        }
        if let Some((f, t)) = hold {
            (shape.hold, text.hold) = (f, t);
        }
        shape.sustain = sustain.unwrap_or(shape.sustain);
        shape.curve = curve.unwrap_or(shape.curve);
        let (target_now, target_text) = target.clone().unwrap_or(now_target.clone());
        let depth_now = depth.unwrap_or(now_depth);

        // on gain the depth is a mix toward silence
        if target_now == GAIN && !(0.0..=1.0).contains(&depth_now) {
            let span = checked.setting("depth").map_or(o.span, |c| c.span);
            return Err(LangErr::new(
                Code::OutOfRange,
                span,
                format!("depth on gain is between 0 and 1, not {depth_now}"),
            )
            .with_hint("elsewhere an env adds depth times its level (to=pitch depth=12)")
            .into());
        }

        let Some(id) = existing else {
            let new = NewEnv {
                shape,
                text,
                target: target_now,
                target_text,
                depth: depth_now,
            };
            if shape.attack == 0
                && shape.decay == 0
                && shape.sustain == 1.0
                && (shape.hold.is_none() || shape.release == 0)
            {
                out.warns.push(LangErr::new(
                    Code::ChangesNothing,
                    o.span,
                    format!(
                        "env '{}' does nothing: it stays at full level (give it an attack or a decay)",
                        name.name
                    ),
                ));
            }
            let (tempo, link) = self.lfo_clock(owner, o.span)?;
            let cmd = self
                .make_env(owner, &name.name, &new, tempo, link)
                .map_err(|e| self.cmd_err(e, o.span))?;
            out.push(cmd);
            return Ok(true);
        };

        // restated: only what differs from the env's values changes
        let mut change = EnvChange::default();
        if shape != now_shape {
            change.shape = Some(shape);
        }
        if text != now_text {
            change.text = Some(text);
        }
        if target_now != now_target.0 {
            change.target = Some((target_now, target_text));
        } else if target_text != now_target.1 {
            // the same target, spelled another way: only the text changes
            change.target_text = Some(target_text);
        }
        if depth_now != now_depth {
            change.depth = Some(depth_now);
        }
        if change.is_empty() {
            out.warn_unchanged(o.span, &format!("'{}'", name.name));
            return Ok(false);
        }
        let cmd = self.edit_env(id, change);
        out.push(cmd);
        Ok(true)
    }

    /// Removes the envs of `owner` that target effect `id`, in the mirror,
    /// and returns the commands for the engine. Each raises W102.
    pub(super) fn drop_envs_of_fx(&mut self, owner: FxOwner, id: u16) -> Vec<Command> {
        let proc_owner = match owner {
            FxOwner::Voice(v) => ProcOwner::Voice(v),
            FxOwner::Group(g) => ProcOwner::Group(g),
        };
        let doomed: Vec<ProcId> = self
            .engine_state
            .owner_procs(proc_owner)
            .map(|p| p.values().copied().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                self.engine_state.procs[p]
                    .env
                    .as_ref()
                    .is_some_and(|e| matches!(e.target, LfoTarget::Fx { id: i, .. } if i == id))
            })
            .collect();
        let mut cmds = Vec::new();
        for p in doomed {
            let path = self.engine_state.proc_path(p).unwrap_or_default();
            let to = self.engine_state.procs[&p]
                .env
                .as_ref()
                .map(|e| e.target_text.clone())
                .unwrap_or_default();
            match self.engine_state.remove_proc(p) {
                Command::Batch(b) => cmds.extend(b.cmds),
                cmd => cmds.push(cmd),
            }
            self.engine_state.warnings.push((
                Code::ReceiverDropped,
                format!("env '{path}' went with the effect it shaped ({to})"),
            ));
        }
        cmds
    }

    /// Removes the envs of voice `v` that target wave `id`, in the mirror,
    /// and returns the commands for the engine. Each raises W102.
    pub(super) fn drop_envs_of_wave(&mut self, v: VoiceId, id: u8) -> Vec<Command> {
        let doomed: Vec<ProcId> =
            self.engine_state
                .owner_procs(ProcOwner::Voice(v))
                .map(|p| p.values().copied().collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .filter(|p| {
                    self.engine_state.procs[p].env.as_ref().is_some_and(
                        |e| matches!(e.target, LfoTarget::Wave { id: i, .. } if i == id),
                    )
                })
                .collect();
        let mut cmds = Vec::new();
        for p in doomed {
            let path = self.engine_state.proc_path(p).unwrap_or_default();
            let to = self.engine_state.procs[&p]
                .env
                .as_ref()
                .map(|e| e.target_text.clone())
                .unwrap_or_default();
            match self.engine_state.remove_proc(p) {
                Command::Batch(b) => cmds.extend(b.cmds),
                cmd => cmds.push(cmd),
            }
            self.engine_state.warnings.push((
                Code::ReceiverDropped,
                format!("env '{path}' went with the wave it shaped ({to})"),
            ));
        }
        cmds
    }
}
