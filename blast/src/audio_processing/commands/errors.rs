//! Errors returned to the command line.

use std::fmt;

/// Why a command line was rejected.
#[derive(Debug)]
pub enum CmdErr {
    TempoFormatting,
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
}

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

    pub(super) fn formatting(err: &str) -> Self {
        CmdErr::Formatting {
            err: err.to_string(),
        }
    }
}

impl fmt::Display for CmdErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CmdErr::TempoFormatting => {
                write!(f, "-t/--tempo must be formatted as unit:interval")
            }
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
        }
    }
}

impl std::error::Error for CmdErr {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_texts() {
        let s = |s: &str| s.to_string();
        let cases = [
            (
                CmdErr::TempoFormatting,
                "-t/--tempo must be formatted as unit:interval",
            ),
            (CmdErr::formatting("as is"), "as is"),
            (CmdErr::missing("name", "load"), "Missing name for 'load'"),
            (
                CmdErr::invalid("x", "seq -c"),
                "Invalid argument 'x' for 'seq -c'",
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
        ];
        for (error, text) in cases {
            assert_eq!(error.to_string(), text);
        }
    }
}
