//! Command parsing (README §3.5): each family file parses its commands
//! against the mirror (`EngineState`) and builds a `Command` for the engine.

mod errors;
pub mod group;
pub mod process;
pub mod procs;
pub mod queue;
pub mod reprs;
pub mod tempo;
pub mod track;
pub mod voice;

#[cfg(test)]
mod testing;

pub use errors::{CmdErr, CmdResult};
pub use group::{GroupArgs, GroupRepr};
pub use process::ProcRepr;
pub use procs::seq::SeqArgs;
pub use queue::CmdQueue;
pub use reprs::{EngineState, Idx};
pub use tempo::{TcArgs, TempoRepr};
pub use track::TrackRepr;
pub use voice::{
    LoadArgs, PauseArgs, ResumeArgs, StartArgs, StopArgs, UnloadArgs, VelocityArgs, VoiceRepr,
};

use blast_macros::var_args;

macro_rules! commands {
    ( $( $var:ident ),* $(,)? ) => {
        /// A validated command for the engine.
        pub enum Command {
            $(
                $var(var_args!($var)), // formats as {CmdType}Args
            )*
        }

        unsafe impl Send for Command {}
        unsafe impl Sync for Command {}
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
    Velocity,
    // Groups
    Group,
    Tc,
    // Processes
    Seq,
    // Program
    Quit,
}

/// Arguments for `Command::Quit`; it only triggers `raise(SIGTERM)`.
pub struct QuitArgs {}

/// One row per command: (name, usage and summary).
pub const HELP: &[(&str, &str)] = &[("quit", "q|quit: exit blast")];

/// What a command line turned into.
pub enum Parsed {
    /// Goes to the engine.
    Engine(Command),
    /// Answered on the command thread (e.g. `ls`, `help`); the text to print.
    Local(String),
}

/// Validates and formats Commands for the engine (string allocations,
/// number parsing, the mirror).
pub struct CmdProcessor {
    pub engine_state: EngineState,
}

impl CmdProcessor {
    pub fn new(engine_state: EngineState) -> Self {
        Self { engine_state }
    }

    /// Parses one command line, updating the mirror on success.
    pub fn parse(&mut self, line: &str) -> CmdResult<Parsed> {
        let (cmd, args) = line.split_once(' ').unwrap_or((line, ""));

        match cmd {
            "load" => self.try_load(args),
            "start" => self.try_start(args),
            "pause" => self.try_pause(args),
            "resume" => self.try_resume(args),
            "stop" => self.try_stop(args),
            "unload" => self.try_unload(args),
            "velocity" => self.try_velocity(args),
            "group" => self.try_group(args),
            "tc" | "tempocon" => self.try_tc(args),
            "seq" => self.try_seq(args),
            "q" | "quit" => Ok(Parsed::Engine(Command::Quit(QuitArgs {}))),
            _ => Err(CmdErr::NoCmd {
                cmd: cmd.to_string(),
            }),
        }
    }

    /// Parses `line` and queues the command for the engine, or returns the
    /// text of a local command. `parse` updates the mirror, so the push must
    /// not fail afterwards: the queue is checked first.
    pub fn submit(&mut self, line: &str, queue: &CmdQueue) -> Result<Option<String>, CmdErr> {
        if queue.is_full() {
            return Err(CmdErr::QueueFull);
        }
        match self.parse(line)? {
            Parsed::Local(text) => Ok(Some(text)),
            Parsed::Engine(cmd) => {
                // can't fail: this thread is the only producer, and the
                // queue had room before parse
                queue.try_push(cmd).map_err(|_| CmdErr::QueueFull)?;
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::audio_processing::commands::testing::*;
    use crate::audio_processing::commands::*;

    #[test]
    fn full_queue_leaves_mirror_unchanged() {
        let mut cp = processor(&["a"]);
        let queue = CmdQueue::new(2); // holds one Command
        assert!(!queue.is_full());

        cp.submit("tc clk b:120", &queue).unwrap();
        assert!(queue.is_full());

        assert!(cp.submit("load a", &queue).is_err());
        assert!(cp.engine_state.voices.is_empty());

        assert!(queue.try_pop().is_some());
        assert!(!queue.is_full());
        cp.submit("load a", &queue).unwrap();
        assert_eq!(v_idx(&cp, "a"), 0);
        assert!(matches!(queue.try_pop(), Some(Command::Load(_))));
        assert!(queue.try_pop().is_none());
    }

    #[test]
    fn submit_reports_parse_errors() {
        let mut cp = processor(&["a"]);
        let queue = CmdQueue::new(4);
        let error = cp.submit("load nope", &queue).unwrap_err();
        assert!(error.to_string().contains("nope"));
        assert!(queue.try_pop().is_none());
    }

    #[test]
    fn empty_line_is_no_cmd() {
        let mut cp = processor(&[]);
        assert!(matches!(err(&mut cp, ""), CmdErr::NoCmd { cmd } if cmd.is_empty()));
    }

    #[test]
    fn submit_returns_none_for_engine_commands() {
        let mut cp = processor(&[]);
        let queue = CmdQueue::new(4);
        assert!(matches!(cp.submit("tc a b:1", &queue), Ok(None)));
        assert!(matches!(queue.try_pop(), Some(Command::Tc(_))));
        assert!(queue.try_pop().is_none());
    }

    #[test]
    fn help_rows_name_real_commands() {
        let tables = [
            HELP,
            voice::HELP,
            group::HELP,
            tempo::HELP,
            track::HELP,
            process::HELP,
            procs::seq::HELP,
        ];
        for (name, usage) in tables.into_iter().flatten() {
            assert!(usage.contains(name), "{name}: {usage}");
            let mut cp = processor(&[]);
            assert!(
                !matches!(cp.parse(name), Err(CmdErr::NoCmd { .. })),
                "{name}"
            );
        }
    }
}
