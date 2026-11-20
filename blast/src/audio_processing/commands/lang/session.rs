//! Incomplete input: a line that ends inside a block, or after `\`, `|`, `and`, `then`, `thenif`
//! or `,`, is kept until the text finishes (REQ-056 §1). The complete text
//! is then lowered as one script.

use super::errors::{Code, LangErr, Source, Span};
use super::parser::{Parse, parse};
use crate::audio_processing::commands::{CmdErr, CmdProcessor, CmdResult, Parsed};

/// What the prompt calls itself in errors.
const PROMPT: &str = "<prompt>";

impl CmdProcessor {
    /// True while an open block or a continued line is buffered and the
    /// next line continues it.
    pub fn pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Drops the pending text (Ctrl-C inside an open block).
    pub fn cancel_pending(&mut self) {
        self.pending.clear();
    }

    /// Parses one line of the language, appended to any pending text. While
    /// the text is incomplete, answers an empty `Local` and keeps it.
    /// Otherwise the whole text is lowered against the mirror (updating it)
    /// as one script; on an error the mirror is unchanged and the pending
    /// text is dropped.
    pub fn parse(&mut self, line: &str) -> CmdResult<Parsed> {
        let text = if self.pending.is_empty() {
            line.to_string()
        } else {
            format!("{}\n{line}", self.pending)
        };
        let src = Source {
            name: PROMPT,
            text: &text,
        };
        match parse(src) {
            Err(errs) => {
                self.pending.clear();
                Err(CmdErr::Lang(errs))
            }
            Ok(Parse::Incomplete) => {
                self.pending = text;
                Ok(Parsed::Local(String::new()))
            }
            Ok(Parse::Complete(script)) => {
                self.pending.clear();
                self.lower_script(&script).map_err(CmdErr::Lang)
            }
        }
    }

    /// Drops pending text; for a batch that ends inside a block, which is an
    /// E202 error.
    pub(in crate::audio_processing::commands) fn unclosed_error(&mut self) -> Option<CmdErr> {
        if self.pending.is_empty() {
            return None;
        }
        self.pending.clear();
        Some(CmdErr::Lang(vec![LangErr::new(
            Code::UnclosedBlock,
            Span {
                line: 1,
                col: 1,
                len: 1,
            },
            "the input ends inside a block or a continued line",
        )]))
    }
}
