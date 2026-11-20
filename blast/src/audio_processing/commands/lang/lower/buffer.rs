//! `buffer` statements (RECb): `buffer NAME [from=] [length=] [loop=]
//! [live=] [bits=] [on CLOCK]`, and `start`, `pause`, `resume`, `stop` and
//! `remove` on a buffer. The memory is set aside here, on the command
//! thread; a restated `length`, `live` or `bits` waits for the next `start`.

use super::*;
use crate::audio_processing::commands::buffer::{BufTo, LenSpec, NewBuffer, Take, no_memory};
use crate::audio_processing::commands::{BufOp, BufferId};
use crate::audio_processing::engine::BufSource;

/// How long a buffer is when `length` isn't given.
const DEFAULT_SECS: f64 = 30.0;

impl CmdProcessor {
    // the length as it was written
    fn buffer_length(value: &Value) -> Res<LenSpec> {
        Ok(match value {
            Value::Num(n) => LenSpec::Ticks(eval_num(n)?),
            Value::Duration(n, unit) => {
                let n = eval_num(n)?;
                match unit {
                    DurUnit::Ticks => LenSpec::Ticks(n),
                    DurUnit::S => LenSpec::Secs(n),
                    DurUnit::Ms => LenSpec::Secs(n / 1e3),
                    DurUnit::Us => LenSpec::Secs(n / 1e6),
                    DurUnit::Ns => LenSpec::Secs(n / 1e9),
                    DurUnit::Frames => LenSpec::Secs(n / frame_rate()),
                    DurUnit::Hz => return Err(hz_is_a_rate(Span::default()).into()),
                }
            }
            _ => LenSpec::Secs(DEFAULT_SECS),
        })
    }

    // a length in frames; ticks need the buffer's clock
    fn buffer_frames(&self, spec: LenSpec, clock: Option<TempoId>, at: Span) -> Res<usize> {
        let secs = match spec {
            LenSpec::Secs(s) => s,
            LenSpec::Ticks(t) => {
                let Some(clock) = clock else {
                    return Err(wrong_type(
                        at,
                        "length=4b needs a clock: add on CLOCK or use s/ms",
                    )
                    .into());
                };
                let (unit, interval) = self
                    .engine_state
                    .tempos
                    .get(&clock)
                    .map(|t| (t.unit, t.interval))
                    .ok_or_else(|| wrong_type(at, "the buffer's clock isn't known"))?;
                t * tick_secs(unit, interval)
            }
        };
        let frames = (secs * frame_rate()).round();
        if !frames.is_finite() || frames < 1.0 {
            return Err(
                LangErr::new(Code::OutOfRange, at, "length must be at least one frame").into(),
            );
        }
        // a float past the range saturates; the allocation then fails (E709)
        Ok(frames as usize)
    }

    // what `from=` names
    fn buffer_source(&self, w: &Ident) -> Res<BufSource> {
        if w.name == OUT {
            return Ok(BufSource::Out);
        }
        let path: Vec<Ident> = w
            .name
            .split('.')
            .map(|name| Ident {
                name: name.to_string(),
                span: w.span,
            })
            .collect();
        match lookup(&self.engine_state, &path)? {
            Named::Voice(v) => Ok(BufSource::Voice(v)),
            Named::Group(g) => Ok(BufSource::Group(g)),
            other => Err(wrong_type(
                w.span,
                format!(
                    "from= takes out, a voice or a group, not {}",
                    article(other.kind())
                ),
            )
            .into()),
        }
    }

    // the clock `on` names
    fn buffer_clock(&self, on: &ClockRef) -> Res<TempoId> {
        let ClockRef::Path(path) = on else {
            return Err(wrong_type(
                clock_ref_span(on),
                "a buffer's 'on' takes the name of a clock",
            )
            .into());
        };
        match lookup(&self.engine_state, path)? {
            Named::Clock(t) => Ok(t),
            other => Err(wrong_type(
                path_span(path),
                format!("'on' takes a clock, not {}", article(other.kind())),
            )
            .into()),
        }
    }

    /// Creates a buffer, or applies a restated one. True if it changed
    /// something.
    pub(super) fn lower_buffer(
        &mut self,
        o: &Object,
        name: &Ident,
        checked: &Checked,
        existing: Option<BufferId>,
        out: &mut Out,
    ) -> Res<bool> {
        if o.block.is_some() {
            return Err(wrong_type(o.span, "buffer takes no block").into());
        }
        let flag = |s: &str| {
            checked
                .setting(s)
                .map(|c| matches!(c.value, Value::Word(w) if w.name == "yes"))
        };
        let bits = match checked.setting("bits") {
            Some(c) => {
                let Value::Num(n) = c.value else {
                    return Err(wrong_type(c.span, "bits takes a number").into());
                };
                let bits = eval_num(n)?;
                if ![8.0, 16.0, 24.0, 32.0].contains(&bits) {
                    return Err(LangErr::new(
                        Code::OutOfRange,
                        c.span,
                        format!("bits must be 8, 16, 24 or 32, not {bits}"),
                    )
                    .into());
                }
                Some(bits as u8)
            }
            None => None,
        };
        // `to=""` is no output; a file's folder must be there
        let to = match checked.setting("to") {
            Some(c) => {
                let Value::Str(written) = c.value else {
                    return Err(wrong_type(c.span, "to takes a quoted string").into());
                };
                let to = BufTo::parse(written, std::env::var_os("HOME"));
                if let Some(why) = to.as_ref().and_then(BufTo::folder_problem) {
                    return Err(LangErr::new(Code::BadSaveFolder, c.span, why).into());
                }
                Some(to)
            }
            None => None,
        };
        let source = match checked.setting("from").map(|c| c.value) {
            Some(Value::Word(w)) => Some(self.buffer_source(w)?),
            _ => None,
        };
        let clock = checked.on.map(|on| self.buffer_clock(on)).transpose()?;
        let length = match checked.setting("length") {
            Some(c) => Some((Self::buffer_length(c.value)?, c.span)),
            None => None,
        };

        let Some(id) = existing else {
            let (spec, at) = length.unwrap_or((LenSpec::Secs(DEFAULT_SECS), o.span));
            let frames = self.buffer_frames(spec, clock, at)?;
            let cmds = self
                .make_buffer(NewBuffer {
                    name: &name.name,
                    source: source.unwrap_or(BufSource::Out),
                    length: spec,
                    frames,
                    clock,
                    looping: flag("loop").unwrap_or(false),
                    live: flag("live").unwrap_or(false),
                    bits: bits.unwrap_or(32),
                    to: to.flatten(),
                })
                .map_err(|e| {
                    let (code, msg) = no_memory(&name.name, e);
                    LangErr::new(code, o.span, msg)
                })?;
            for cmd in cmds {
                out.push(cmd);
            }
            return Ok(true);
        };

        // a restatement: what differs is applied, what is the same is left
        let Some(repr) = self.engine_state.buffer_by_id(id).cloned() else {
            return Ok(false);
        };
        let mut changed = false;
        if let Some(c) = clock
            && Some(c) != repr.clock
            && let Some(r) = self.engine_state.buffers.get_mut(&repr.name)
        {
            r.clock = Some(c);
            changed = true;
        }
        // a new length needs the clock as it is after this statement
        let frames = match length {
            Some((spec, at)) => Some((spec, self.buffer_frames(spec, clock.or(repr.clock), at)?)),
            None => None,
        };
        if let Some(src) = source
            && src != repr.source
        {
            for cmd in self.set_buffer_source(&repr.name, src) {
                out.push(cmd);
            }
            changed = true;
        }
        if let Some(on) = flag("loop")
            && on != repr.looping
            && let Some(cmd) = self.buffer_transport(&repr.name, BufOp::Looping(on))
        {
            out.push(cmd);
            changed = true;
        }
        if let Some(to) = to
            && to != repr.to
            && let Some(r) = self.engine_state.buffers.get_mut(&repr.name)
        {
            r.to = to;
            changed = true;
        }
        // these three wait for the next start (fresh memory then)
        if let Some(r) = self.engine_state.buffers.get_mut(&repr.name) {
            if let Some((spec, frames)) = frames
                && spec != r.length
            {
                r.length = spec;
                r.frames = frames;
                changed = true;
            }
            if let Some(live) = flag("live")
                && live != r.live
            {
                r.live = live;
                changed = true;
            }
            if let Some(bits) = bits
                && bits != r.bits
            {
                r.bits = bits;
                changed = true;
            }
        }
        Ok(changed)
    }

    /// `start`, `pause`, `resume` or `stop` on a buffer. One that has
    /// nothing to do is a no-op (W101).
    pub(super) fn buffer_action(
        &mut self,
        word: ActionWord,
        id: BufferId,
        at: Span,
        out: &mut Out,
    ) -> Res<()> {
        let Some(repr) = self.engine_state.buffer_by_id(id).cloned() else {
            return Ok(());
        };
        let (op, ok, why) = match word {
            ActionWord::Start => (
                BufOp::Start,
                repr.take == Take::Idle,
                "is already recording",
            ),
            ActionWord::Pause => (
                BufOp::Pause,
                repr.take == Take::Recording,
                "isn't recording",
            ),
            ActionWord::Resume => (BufOp::Resume, repr.take == Take::Paused, "isn't paused"),
            ActionWord::Stop => (BufOp::Stop, repr.take != Take::Idle, "isn't recording"),
            _ => return Ok(()),
        };
        if !ok {
            out.warns.push(LangErr::new(
                Code::ChangesNothing,
                at,
                format!("buffer '{}' {why}", repr.name),
            ));
            return Ok(());
        }
        if op == BufOp::Start && repr.stale() {
            // a restated length, live or bits: fresh memory first, and the
            // voices playing the buffer move to it
            let cmds = self.renew_buffer(&repr.name).map_err(|e| {
                let (code, msg) = no_memory(&repr.name, e);
                LangErr::new(code, at, msg)
            })?;
            for cmd in cmds {
                out.push(cmd);
            }
        }
        // a take with `to=` carries a private copy of itself
        let save = if op == BufOp::Start {
            if let Some(why) = repr.to.as_ref().and_then(BufTo::folder_problem) {
                return Err(LangErr::new(Code::BadSaveFolder, at, why).into());
            }
            self.save_box(&repr.name).map_err(|e| {
                let (code, msg) = no_memory(&repr.name, e);
                LangErr::new(code, at, msg)
            })?
        } else {
            None
        };
        if let Some(mut cmd) = self.buffer_transport(&repr.name, op) {
            if let Command::BufferEdit(a) = &mut cmd {
                a.save = save;
            }
            out.push(cmd);
        }
        Ok(())
    }

    /// `remove NAME` for a buffer.
    pub(super) fn remove_buffer_target(&mut self, id: BufferId) -> Command {
        let name = self
            .engine_state
            .buffer_by_id(id)
            .map(|b| b.name.clone())
            .unwrap_or_default();
        Command::Batch(BatchArgs {
            cmds: self.remove_buffer(&name),
        })
    }
}
