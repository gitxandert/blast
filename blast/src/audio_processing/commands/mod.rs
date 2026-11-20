//! Command parsing (README §3.5): `lang/` parses and lowers the language
//! against the mirror (`EngineState`); the family files hold the typed cores
//! it calls to build a `Command` for the engine.

pub mod buffer;
mod errors;
pub mod fx;
pub mod group;
pub mod inspect;
pub mod lang;
mod named_ids;
pub mod process;
pub mod procs;
pub mod queue;
pub mod reprs;
pub mod tempo;
pub mod track;
pub mod voice;
pub mod wave;

#[cfg(test)]
mod testing;

pub use buffer::{
    BufOp, BufSaves, BufTo, BufferArgs, BufferEditArgs, BufferRepr, LenSpec, SetSourceArgs,
    SetTapsArgs, Take, TapOwner, ToDest,
};
pub use errors::{CmdErr, CmdResult, RESERVED_ALL};
pub use fx::{Bypass, FxArgs, FxOp, FxOwner, FxRepr};
pub use group::{GroupArgs, GroupEditArgs, GroupGainArgs, GroupOp, GroupRepr};
pub use named_ids::{
    AnyId, cannot_stage, created_ids, creates_id, creates_or_removes, effect_ids, fm_modulator_ids,
    glide_refs, holds_no_statement, names_id, visit_ids,
};
pub use process::{ProcEditArgs, ProcOp, ProcRepr, SeqRepr};
pub use procs::env::{EnvArgs, EnvEditArgs, EnvRepr};
pub use procs::lfo::{LfoArgs, LfoEditArgs, LfoRepr};
pub use procs::packet::PacketRepr;
pub use procs::seq::{
    FOLLOWS_CHANCE, FOLLOWS_JITTER, FeelEdit, Pending, RecvSpec, SeqArgs, SeqEditArgs, SeqRecvArgs,
    SeqStep, SeqVecs, TrigTarget,
};
pub use queue::{CmdReceiver, CmdSender, cmd_queue};
pub use reprs::{
    BufferId, EngineState, GroupId, IdKind, IdPool, Ids, PacketId, ProcId, ProcOwner, Target,
    TempoId, TempoLink, TempoOwner, TrackId, VoiceId,
};
pub use tempo::{
    ClockRun, TcArgs, TempoAssignArgs, TempoFreeArgs, TempoInit, TempoRepr, TempoSetArgs,
    TempoTarget, TempoUse,
};
pub use track::{AddTrackArgs, DropTrackArgs, TrackRepr, TrackSource};
pub use voice::{
    LoadArgs, LoadSource, PauseArgs, ResampleMode, ResumeArgs, SetModeArgs, SetParamArgs,
    SetShiftersArgs, SetTrackArgs, ShiftMode, StartArgs, StopArgs, UnloadArgs, VoiceMode,
    VoiceRepr, VoiceSrc,
};
pub use wave::{DEFAULT_TABLE_FRAMES, DEFAULT_TABLE_SLICES, WaveArgs, WaveFrom, WaveOp, WaveRepr};

use crate::audio_processing::blast_time::blast_time::TempoUnit;
use blast_macros::var_args;

macro_rules! commands {
    ( $( $var:ident ),* $(,)? ) => {
        /// A validated command for the engine.
        pub enum Command {
            $(
                $var(var_args!($var)), // formats as {CmdType}Args
            )*
        }
    }
}

commands! {
    // Voices
    Load,
    Start,
    Pause,
    Resume,
    Stop,
    Unload,
    SetParam,
    SetMode,
    SetTrack,
    SetShifters,
    // Tracks
    AddTrack,
    DropTrack,
    // Groups
    Group,
    GroupEdit,
    GroupGain,
    Tc,
    TempoSet,
    TempoFree,
    TempoAssign,
    // Processes
    Seq,
    SeqEdit,
    SeqRecv,
    Lfo,
    LfoEdit,
    Env,
    EnvEdit,
    ProcEdit,
    // Effects
    Fx,
    // Waves
    Wave,
    // Buffers
    Buffer,
    BufferEdit,
    SetTaps,
    SetSource,
    // Program
    Schedule,
    MarkGlides,
    SetMark,
    CancelScript,
    Batch,
    Quit,
}

/// When a scheduled line applies, on the clock `tempo` (SCHED). The engine
/// turns it into a position of that clock when the command arrives.
#[derive(Clone, Debug, PartialEq)]
pub enum WhenSpec {
    /// On the next multiple of `every` ticks (never now).
    At { tempo: TempoId, every: f64 },
    /// `ticks` ticks from now.
    After { tempo: TempoId, ticks: f64 },
    /// `then` and `thenif`: when the glides started since the last
    /// `MarkGlides` are due. The engine resolves it when the Schedule applies,
    /// as the frame now plus the longest of them. `watch` is `Some` for
    /// `thenif`: the glides that must run their course. The command thread
    /// builds the list with generation 0; the engine fills the generations in
    /// when the Schedule applies.
    AfterGlides { watch: Option<Box<[GlideWatch]>> },
    /// A segment of a timed script (PKb): once mark `seq` of script slot
    /// `slot` is set (`SetMark`), `ticks` ticks of `tempo` after the position
    /// the mark recorded.
    AfterMark {
        slot: u8,
        seq: u32,
        tempo: TempoId,
        ticks: f64,
    },
}

/// A glide a `thenif` checks, and the generation of its slot when the
/// `thenif` was applied. A later generation means the glide was replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlideWatch {
    pub glide: GlideRef,
    pub generation: u32,
}

/// Which glide: the slot a setting glides in. Each slot keeps a generation
/// that changes whenever something starts a glide on it or sets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlideRef {
    Voice {
        voice: VoiceId,
        param: crate::audio_processing::engine::VoiceParam,
    },
    Group(GroupId),
    Tempo(TempoId),
    Fx {
        owner: FxOwner,
        id: u16,
        param: u8,
    },
    /// A parameter of one of a voice's waves.
    Wave {
        voice: VoiceId,
        id: u8,
        param: u8,
    },
    /// An lfo's `depth`, `offset` or `freq`, or a seq's `chance`, `early` or
    /// `late`: all glides of the process count as one slot.
    Proc(ProcId),
}

/// Arguments for `Command::Schedule`: `cmd` waits in the engine's schedule
/// until `when`. The box is built on the command thread and leaves through
/// the garbage queue.
pub struct ScheduleArgs {
    pub when: WhenSpec,
    pub cmd: Box<Command>,
    /// The number the mirror gave this entry (its place in the line's tree,
    /// outermost first); `Event::ScheduleFired` repeats it.
    pub seq_no: u64,
    /// The running script this entry belongs to (its mark slot), so that
    /// `CancelScript` can drop it.
    pub script: Option<u8>,
}

/// Arguments for `Command::SetMark`: script `slot` has reached its segment
/// `seq`. The engine records where `tempo` (the clock the next wait counts
/// on) is now; `AfterMark` entries count from it.
pub struct SetMarkArgs {
    pub slot: u8,
    pub seq: u32,
    pub tempo: TempoId,
}

/// Arguments for `Command::CancelScript`: drops every waiting entry that
/// belongs to script `slot`, forgets its mark, and applies `stops`.
pub struct CancelScriptArgs {
    pub slot: u8,
    /// `Stop` commands for what the script started (`end`); the engine
    /// applies those whose target still exists. Empty for `source NAME stop`.
    pub stops: Vec<Command>,
}

/// Arguments for `Command::MarkGlides`: the engine forgets the glides it has
/// started, so the next `AfterGlides` counts only the ones after this.
pub struct MarkGlidesArgs {}

/// Arguments for `Command::Batch`: commands applied in order, within one
/// period.
pub struct BatchArgs {
    pub cmds: Vec<Command>,
}

/// Arguments for `Command::Quit`; it makes the engine emit `Event::Quit`.
pub struct QuitArgs {}

/// What a command line turned into.
pub enum Parsed {
    /// Goes to the engine.
    Engine(Command),
    /// Answered on the command thread (e.g. a warning, `state`, `help`); the
    /// text to print.
    Local(String),
}

/// Validates and formats Commands for the engine (string allocations,
/// number parsing, the mirror).
pub struct CmdProcessor {
    pub engine_state: EngineState,
    /// Text of an unfinished block or continued line (`lang::session`).
    pending: String,
    /// `let` variables (`lang::vars`); they live for the session.
    vars: lang::vars::Vars,
    /// The canonical paths of the files being sourced, outermost first
    /// (`lang::script`).
    src_stack: Vec<std::path::PathBuf>,
    /// What queries answered in a line that also made commands; `submit`
    /// prints it.
    outbox: String,
    /// Set by a `clear` typed at the prompt; the REPL takes it after `submit`.
    pub clear_requested: bool,
    /// Statements a `source … history` produced, for the REPL to add to
    /// its history (taken like `clear_requested`).
    pub history_out: Vec<String>,
    /// Where bare `source` names are looked up and `state to` writes
    /// (`--sources`).
    pub sources_dir: std::path::PathBuf,
    /// True in the copy a script is checked against: nothing is written.
    checking: bool,
    /// The timed scripts that are running (`lang::runner`).
    runner: lang::runner::Runner,
}

impl CmdProcessor {
    pub fn new(engine_state: EngineState) -> Self {
        Self {
            engine_state,
            pending: String::new(),
            vars: lang::vars::Vars::default(),
            src_stack: Vec::new(),
            outbox: String::new(),
            clear_requested: false,
            history_out: Vec::new(),
            sources_dir: std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/sources")),
            checking: false,
            runner: lang::runner::Runner::default(),
        }
    }

    /// Sets the sources folder (`--sources`).
    pub fn set_sources_dir(&mut self, dir: std::path::PathBuf) {
        self.sources_dir = dir;
    }

    /// Creates the session's clock `main` (120b) in the mirror (G2) and
    /// returns the command that creates and starts it in the engine; the
    /// runtime queues it before anything else.
    pub fn boot(&mut self) -> Command {
        match self.make_clock(reprs::MAIN, TempoUnit::Bpm, 120.0) {
            Ok(Command::Tc(args)) => {
                self.engine_state
                    .run_transport(lang::ast::ActionWord::Start, Target::Tempo(args.tempo.id));
                let start = Command::Start(StartArgs {
                    target: Target::Tempo(args.tempo.id),
                });
                Command::Batch(BatchArgs {
                    cmds: vec![Command::Tc(args), start],
                })
            }
            // no tempo slot for it: an empty Batch, and `main` is missing
            _ => Command::Batch(BatchArgs { cmds: Vec::new() }),
        }
    }

    /// Parses `line` and queues the command for the engine, or returns the
    /// text of a local command. `parse` updates the mirror, so the push must
    /// not fail afterwards: the queue is checked first.
    pub fn submit(&mut self, line: &str, queue: &mut CmdSender) -> Result<Option<String>, CmdErr> {
        if queue.is_full() {
            return Err(CmdErr::QueueFull);
        }
        match self.parse(line)? {
            // an incomplete line answers an empty text: nothing to print
            Parsed::Local(text) if text.is_empty() => Ok(None),
            Parsed::Local(text) => Ok(Some(text)),
            Parsed::Engine(cmd) => {
                // can't fail: this thread is the only producer, and the
                // queue had room before parse
                queue.try_push(cmd).map_err(|_| CmdErr::QueueFull)?;
                let text = std::mem::take(&mut self.outbox);
                Ok((!text.is_empty()).then_some(text))
            }
        }
    }

    /// Parses every line in order against the mirror as it evolves; on
    /// success queues one Command (a Batch if more than one). On a parse
    /// error or a full queue, the mirror is exactly as before. `Local`
    /// outputs are joined with newlines (REQ-007).
    pub fn submit_batch(
        &mut self,
        lines: &[String],
        queue: &mut CmdSender,
    ) -> Result<Option<String>, CmdErr> {
        if queue.is_full() {
            return Err(CmdErr::QueueFull);
        }
        let (cmd, text) = self.parse_batch(lines, Self::parse)?;
        if let Some(cmd) = cmd {
            // can't fail: this thread is the only producer, and the queue
            // had room before parsing
            queue.try_push(cmd).map_err(|_| CmdErr::QueueFull)?;
        }
        Ok(text)
    }

    // submit_batch without the queue; `parse` is a parameter so tests can
    // add local commands
    fn parse_batch(
        &mut self,
        lines: &[String],
        mut parse: impl FnMut(&mut Self, &str) -> CmdResult<Parsed>,
    ) -> CmdResult<(Option<Command>, Option<String>)> {
        let snapshot = (
            self.engine_state.clone(),
            self.vars.clone(),
            self.runner.clone(),
        );
        let mut cmds = Vec::new();
        let mut texts = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            match parse(self, line) {
                Ok(Parsed::Engine(cmd)) => {
                    match cmd {
                        Command::Batch(args) => cmds.extend(args.cmds),
                        cmd => cmds.push(cmd),
                    }
                    let text = std::mem::take(&mut self.outbox);
                    if !text.is_empty() {
                        texts.push(text);
                    }
                }
                Ok(Parsed::Local(text)) if text.is_empty() => {}
                Ok(Parsed::Local(text)) => texts.push(text),
                Err(err) => {
                    (self.engine_state, self.vars, self.runner) = snapshot;
                    return Err(CmdErr::Line {
                        line: i + 1,
                        err: Box::new(err),
                    });
                }
            }
        }

        // a batch can't end inside a block
        if let Some(err) = self.unclosed_error() {
            (self.engine_state, self.vars, self.runner) = snapshot;
            return Err(CmdErr::Line {
                line: lines.len(),
                err: Box::new(err),
            });
        }

        let cmd = match cmds.len() {
            0 => None,
            1 => cmds.pop(),
            _ => Some(Command::Batch(BatchArgs { cmds })),
        };
        let text = (!texts.is_empty()).then(|| texts.join("\n"));
        Ok((cmd, text))
    }
}

#[cfg(test)]
mod tests {
    use crate::audio_processing::commands::lang::Code;
    use crate::audio_processing::commands::testing::*;
    use crate::audio_processing::commands::*;

    #[test]
    fn full_queue_leaves_mirror_unchanged() {
        let mut cp = processor(&["a"]);
        let (mut tx, mut rx) = cmd_queue(1); // holds one Command
        assert!(!tx.is_full());

        cp.submit("clock clk 120b", &mut tx).unwrap();
        assert!(tx.is_full());

        assert!(cp.submit("voice a from \"a\" on 1000ms", &mut tx).is_err());
        assert!(cp.engine_state.voices.is_empty());

        assert!(rx.try_pop().is_some());
        assert!(!tx.is_full());
        cp.submit("voice a from \"a\" on 1000ms", &mut tx).unwrap();
        assert_eq!(vid(&cp, "a"), VoiceId(0));
        assert!(matches!(rx.try_pop(), Some(Command::Load(_))));
        assert!(rx.try_pop().is_none());
    }

    #[test]
    fn submit_reports_parse_errors() {
        let mut cp = processor(&["a"]);
        let (mut tx, mut rx) = cmd_queue(3);
        let error = cp
            .submit("voice a from \"nope\" on 1000ms", &mut tx)
            .unwrap_err();
        assert!(error.to_string().contains("nope"));
        assert!(rx.try_pop().is_none());
    }

    #[test]
    fn empty_line_does_nothing() {
        let mut cp = processor(&[]);
        assert_eq!(local(&mut cp, ""), "");
        assert_eq!(local(&mut cp, "# only a comment"), "");
    }

    #[test]
    fn no_flag_syntax_left() {
        // the old flag syntax is not a statement any more
        let mut cp = processor(&["kick"]);
        for line in [
            "load kick",
            "unload kick",
            "tc x b:120",
            "group g -v kick",
            "velocity kick 2",
            "seq kick -s 0,1",
            "start -v kick",
        ] {
            let errs = lerr(&mut cp, line);
            assert!(
                matches!(errs[0].code, Code::Unexpected | Code::UnknownName),
                "{line}: {}",
                errs[0]
            );
        }
        assert!(cp.engine_state.voices.is_empty());
        assert!(cp.engine_state.tempos.is_empty());
    }

    #[test]
    fn submit_returns_none_for_engine_commands() {
        let mut cp = processor(&[]);
        let (mut tx, mut rx) = cmd_queue(3);
        assert!(matches!(cp.submit("clock a 1b", &mut tx), Ok(None)));
        assert!(matches!(rx.try_pop(), Some(Command::Tc(_))));
        assert!(rx.try_pop().is_none());
    }

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn submit_batch_queues_one_batch() {
        let mut cp = processor(&["a", "b"]);
        let (mut tx, mut rx) = cmd_queue(3);
        let out = cp.submit_batch(
            &lines(&[
                "voice a from \"a\" on 1000ms",
                "voice b from \"b\" on 1000ms",
                "start a",
            ]),
            &mut tx,
        );
        assert!(matches!(out, Ok(None)));
        let Some(Command::Batch(args)) = rx.try_pop() else {
            panic!("expected one Batch");
        };
        assert!(rx.try_pop().is_none());
        assert!(matches!(
            args.cmds.as_slice(),
            [Command::Load(_), Command::Load(_), Command::Start(_)]
        ));
        assert_eq!(vid(&cp, "b"), VoiceId(1));
    }

    #[test]
    fn failed_batch_leaves_mirror_unchanged() {
        let mut cp = processor(&["a", "b"]);
        let (mut tx, mut rx) = cmd_queue(3);
        let error = cp
            .submit_batch(
                &lines(&[
                    "voice a from \"a\" on 1000ms",
                    "voice b from \"b\" on 1000ms",
                    "voice c from \"nope\" on 1000ms",
                ]),
                &mut tx,
            )
            .unwrap_err();
        assert!(matches!(&error, CmdErr::Line { line: 3, .. }));
        assert!(error.to_string().starts_with("line 3: "), "{error}");
        assert!(error.to_string().contains("nope"));
        assert!(rx.try_pop().is_none());

        let es = &cp.engine_state;
        assert!(es.voices.is_empty());
        assert!(es.voice_names.is_empty());
        assert!(es.tempos.is_empty());
        assert_eq!(es.ids.voices.live(), 0);
        assert_eq!(es.ids.tempos.live(), 0);
        // the pools hand out the same IDs as before the batch
        cp.submit("voice b from \"b\" on 1000ms", &mut tx).unwrap();
        assert_eq!(vid(&cp, "b"), VoiceId(0));
        assert_eq!(cp.engine_state.voices[&VoiceId(0)].tempo, TempoId(0));
    }

    #[test]
    fn batch_lines_see_earlier_lines() {
        let mut cp = processor(&["a"]);
        let (mut tx, mut rx) = cmd_queue(3);
        cp.submit_batch(
            &lines(&["voice a from \"a\" on 1000ms", "a { seq s 1 }"]),
            &mut tx,
        )
        .unwrap();
        let Some(Command::Batch(args)) = rx.try_pop() else {
            panic!("expected one Batch");
        };
        assert!(matches!(
            args.cmds.as_slice(),
            [Command::Load(_), Command::Seq(_)]
        ));
        assert_eq!(cp.engine_state.procs.len(), 1);
    }

    #[test]
    fn single_line_batch_is_plain_command() {
        let mut cp = processor(&["a"]);
        let (mut tx, mut rx) = cmd_queue(3);
        cp.submit_batch(&lines(&["voice a from \"a\" on 1000ms"]), &mut tx)
            .unwrap();
        assert!(matches!(rx.try_pop(), Some(Command::Load(_))));
        assert!(rx.try_pop().is_none());

        // an empty batch queues nothing
        assert!(matches!(cp.submit_batch(&[], &mut tx), Ok(None)));
        assert!(rx.try_pop().is_none());
    }

    #[test]
    fn batch_local_output_is_returned() {
        // no command is answered locally yet (LANGd adds `state` & co.), so
        // this one stands in for them
        let mut cp = processor(&["a"]);
        let parse = |cp: &mut CmdProcessor, line: &str| match line {
            "ls" => Ok(Parsed::Local("x".to_string())),
            _ => cp.parse(line),
        };
        let (cmd, text) = cp
            .parse_batch(&lines(&["ls", "voice a from \"a\" on 1000ms", "ls"]), parse)
            .unwrap();
        assert!(matches!(cmd, Some(Command::Load(_))));
        assert_eq!(text.as_deref(), Some("x\nx"));

        let (cmd, text) = cp.parse_batch(&lines(&["ls"]), parse).unwrap();
        assert!(cmd.is_none());
        assert_eq!(text.as_deref(), Some("x"));
    }

    #[test]
    fn full_queue_rejects_batch_untouched() {
        let mut cp = processor(&["a", "b"]);
        let (mut tx, mut rx) = cmd_queue(1); // holds one Command
        cp.submit("clock clk 120b", &mut tx).unwrap();
        assert!(matches!(
            cp.submit_batch(
                &lines(&[
                    "voice a from \"a\" on 1000ms",
                    "voice b from \"b\" on 1000ms"
                ]),
                &mut tx
            ),
            Err(CmdErr::QueueFull)
        ));
        assert!(cp.engine_state.voices.is_empty());
        assert_eq!(cp.engine_state.ids.voices.live(), 0);
        assert!(matches!(rx.try_pop(), Some(Command::Tc(_))));
        assert!(rx.try_pop().is_none());
    }
}
