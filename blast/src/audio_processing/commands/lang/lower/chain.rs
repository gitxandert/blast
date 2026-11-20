//! The shape a chain of `then`, `thenif` and `wait` takes on its way to the
//! engine (PKa). The parts are already lowered; this only arranges them as
//! `Schedule`s.
//!
//! `a and b then c and d then e` becomes, left to right:
//!
//! ```text
//! MarkGlides, a, b,
//! Schedule(AfterGlides, [MarkGlides])          the "head"
//! Schedule(AfterGlides, c)                     one entry per statement
//! Schedule(AfterGlides, d)
//! Schedule(AfterGlides, [ ...how e follows... ])   the "tail"
//! ```
//!
//! All of them resolve to the same frame (the longest glide of `a` and `b`),
//! and fire in the order they were sent. The head resets the engine's glide
//! mark just before `c` and `d` start their glides, and the tail (which holds
//! no statement of its own, so a removed object can only take the statement
//! that names it) schedules `e` once they have all started, resolving to the
//! longest of those. `thenif` is one entry for everything after it, since a
//! cut-short glide drops the whole rest; a `wait` is an entry that holds
//! nothing but the rest, or, at the start, is simply the timing of the first
//! entries.

use crate::audio_processing::commands::{
    BatchArgs, Command, GlideRef, GlideWatch, ScheduleArgs, TempoId, WhenSpec, glide_refs,
};

use super::super::ast::Join;

/// A part of a chain after its statements were lowered.
pub(super) enum Lowered {
    /// The commands of each statement.
    Stmts(Vec<Vec<Command>>),
    /// Does nothing; done after `ticks` of `tempo`.
    Wait { tempo: TempoId, ticks: f64 },
}

// how the entries that follow a part are timed
#[derive(Clone, Copy)]
enum Timing {
    /// When the glides started since the last `MarkGlides` are over.
    Glides,
    /// After a wait.
    After { tempo: TempoId, ticks: f64 },
}

pub(super) struct Chain {
    parts: Vec<Option<Lowered>>,
    joins: Vec<Join>,
    // what each part's commands glide, for a `thenif` after it
    glides: Vec<Vec<GlideRef>>,
}

impl Chain {
    pub(super) fn new(parts: Vec<Lowered>, joins: Vec<Join>) -> Self {
        let glides = parts
            .iter()
            .map(|p| {
                let mut refs = Vec::new();
                if let Lowered::Stmts(each) = p {
                    for cmd in each.iter().flatten() {
                        glide_refs(cmd, &mut refs);
                    }
                }
                refs
            })
            .collect();
        Chain {
            parts: parts.into_iter().map(Some).collect(),
            joins,
            glides,
        }
    }

    /// A `Schedule` of `cmds` (one command or a `Batch`).
    pub(super) fn schedule(when: WhenSpec, mut cmds: Vec<Command>) -> Command {
        let cmd = match cmds.len() {
            1 => cmds.remove(0),
            _ => Command::Batch(BatchArgs { cmds }),
        };
        Command::Schedule(ScheduleArgs {
            when,
            cmd: Box::new(cmd),
            seq_no: 0,
            script: None,
        })
    }

    fn entry(timing: Timing, cmds: Vec<Command>) -> Command {
        let when = match timing {
            Timing::Glides => WhenSpec::AfterGlides { watch: None },
            Timing::After { tempo, ticks } => WhenSpec::After { tempo, ticks },
        };
        Self::schedule(when, cmds)
    }

    /// The commands that carry out part `i` and everything after it, at the
    /// moment part `i` begins.
    pub(super) fn run(&mut self, i: usize) -> Vec<Command> {
        let more = i + 1 < self.parts.len();
        match self.parts[i].take() {
            Some(Lowered::Stmts(each)) => {
                let mut out = Vec::new();
                if more {
                    out.push(Command::MarkGlides(
                        crate::audio_processing::commands::MarkGlidesArgs {},
                    ));
                }
                out.extend(each.into_iter().flatten());
                if more {
                    out.extend(self.follow(i + 1, Timing::Glides));
                }
                out
            }
            Some(Lowered::Wait { tempo, ticks }) if more => {
                self.follow(i + 1, Timing::After { tempo, ticks })
            }
            _ => Vec::new(),
        }
    }

    // the entries that start part `i` (`i > 0`) and all that follows, when
    // `timing` says
    fn follow(&mut self, i: usize, timing: Timing) -> Vec<Command> {
        let join = self.joins[i - 1];
        let more = i + 1 < self.parts.len();
        if matches!(self.parts[i], Some(Lowered::Wait { .. })) {
            // an entry with only the rest in it
            let body = self.run(i);
            return if body.is_empty() {
                body
            } else {
                vec![Self::entry(timing, body)]
            };
        }
        // `thenif` after a wait always succeeds: there is no glide to cut short
        if matches!(join, Join::ThenIf(_)) && matches!(timing, Timing::Glides) {
            let watch: Vec<GlideWatch> = self.glides[i - 1]
                .iter()
                .map(|&glide| GlideWatch {
                    glide,
                    generation: 0,
                })
                .collect();
            let body = self.run(i);
            if body.is_empty() {
                return body;
            }
            let watch = (!watch.is_empty()).then(|| watch.into_boxed_slice());
            return vec![Self::schedule(WhenSpec::AfterGlides { watch }, body)];
        }
        let Some(Lowered::Stmts(each)) = self.parts[i].take() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if more {
            out.push(Self::entry(
                timing,
                vec![Command::MarkGlides(
                    crate::audio_processing::commands::MarkGlidesArgs {},
                )],
            ));
        }
        for cmds in each.into_iter().filter(|c| !c.is_empty()) {
            out.push(Self::entry(timing, cmds));
        }
        if more {
            let rest = self.follow(i + 1, Timing::Glides);
            if !rest.is_empty() {
                out.push(Self::entry(timing, rest));
            }
        }
        out
    }
}
