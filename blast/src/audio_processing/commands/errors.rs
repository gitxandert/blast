//! Errors returned to the command line.

use super::lang::LangErr;
use std::fmt;

/// Why a command line was rejected.
#[derive(Debug)]
pub enum CmdErr {
    /// Generic; the message must explain itself in context.
    Formatting {
        err: String,
    },
    MissingArg {
        arg: String,
        cmd: String,
    },
    InvalidArg {
        arg: String,
        cmd: String,
    },
    AlreadyIs {
        ty: String,
        name: String,
    },
    NoCmd {
        cmd: String,
    },
    NoItem {
        ty: String,
        name: String,
    },
    NoVoice {
        name: String,
        group: Option<String>,
    },
    QueueFull,
    /// An ID pool is full; nothing was queued.
    LimitReached {
        kind: &'static str,
        limit: u32,
    },
    /// A name the command layer keeps for itself (`all`, REQ-006).
    Reserved {
        name: String,
    },
    /// Errors from the language (LANGb): every one is printed on its own line.
    Lang(Vec<LangErr>),
    /// Line `line` (from 1) of a batch failed; nothing was queued.
    Line {
        line: usize,
        err: Box<CmdErr>,
    },
}

/// The name that stands for every object of a kind (REQ-006); no new
/// object may take it.
pub const RESERVED_ALL: &str = "all";

/// The result of parsing a command line.
pub type CmdResult<T> = Result<T, CmdErr>;

impl CmdErr {
    /// `arg` is missing from `cmd`.
    pub fn missing(arg: &str, cmd: &str) -> Self {
        CmdErr::MissingArg {
            arg: arg.to_string(),
            cmd: cmd.to_string(),
        }
    }

    /// `arg` isn't valid for `cmd`.
    pub fn invalid(arg: &str, cmd: &str) -> Self {
        CmdErr::InvalidArg {
            arg: arg.to_string(),
            cmd: cmd.to_string(),
        }
    }
}

impl fmt::Display for CmdErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CmdErr::Formatting { err } => write!(f, "{err}"),
            CmdErr::MissingArg { arg, cmd } => write!(f, "Missing {arg} for '{cmd}'"),
            CmdErr::InvalidArg { arg, cmd } => write!(f, "Invalid argument '{arg}' for '{cmd}'"),
            CmdErr::AlreadyIs { ty, name } => write!(f, "Already a {ty} called '{name}'"),
            CmdErr::NoCmd { cmd } => write!(f, "Invalid command '{cmd}'"),
            CmdErr::NoItem { ty, name } => write!(f, "Couldn't find {ty} '{name}'"),
            CmdErr::NoVoice { name, group } => match group {
                Some(g_name) => write!(f, "Couldn't find Voice '{name}' in Group '{g_name}'"),
                None => write!(f, "Couldn't find Voice '{name}'"),
            },
            CmdErr::QueueFull => write!(f, "Command queue full"),
            CmdErr::LimitReached { kind, limit } => {
                write!(f, "Too many {kind} (limit {limit})")
            }
            CmdErr::Reserved { name } => write!(f, "'{name}' is reserved"),
            CmdErr::Line { line, err } => write!(f, "line {line}: {err}"),
            CmdErr::Lang(errs) => {
                for (i, e) in errs.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{e}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for CmdErr {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::commands::lang::{Code, Span};

    #[test]
    fn display_texts() {
        let s = |s: &str| s.to_string();
        let cases = [
            (CmdErr::Formatting { err: s("as is") }, "as is"),
            (CmdErr::missing("name", "load"), "Missing name for 'load'"),
            (
                CmdErr::invalid("x", "clock"),
                "Invalid argument 'x' for 'clock'",
            ),
            (
                CmdErr::AlreadyIs {
                    ty: s("Group"),
                    name: s("g"),
                },
                "Already a Group called 'g'",
            ),
            (CmdErr::NoCmd { cmd: s("x") }, "Invalid command 'x'"),
            (
                CmdErr::NoItem {
                    ty: s("track"),
                    name: s("k"),
                },
                "Couldn't find track 'k'",
            ),
            (
                CmdErr::NoVoice {
                    name: s("v"),
                    group: Some(s("g")),
                },
                "Couldn't find Voice 'v' in Group 'g'",
            ),
            (
                CmdErr::NoVoice {
                    name: s("v"),
                    group: None,
                },
                "Couldn't find Voice 'v'",
            ),
            (CmdErr::QueueFull, "Command queue full"),
            (
                CmdErr::LimitReached {
                    kind: "voices",
                    limit: 2,
                },
                "Too many voices (limit 2)",
            ),
            (CmdErr::Reserved { name: s("all") }, "'all' is reserved"),
            (
                CmdErr::Line {
                    line: 3,
                    err: Box::new(CmdErr::QueueFull),
                },
                "line 3: Command queue full",
            ),
            (
                CmdErr::Lang(vec![
                    LangErr::new(
                        Code::UnknownName,
                        Span {
                            line: 1,
                            col: 2,
                            len: 1,
                        },
                        "a",
                    ),
                    LangErr::new(
                        Code::QueueFull,
                        Span {
                            line: 2,
                            col: 1,
                            len: 1,
                        },
                        "b",
                    ),
                ]),
                "1:2: E301 a\n2:1: E602 b",
            ),
        ];
        for (error, text) in cases {
            assert_eq!(error.to_string(), text);
        }
    }
}
