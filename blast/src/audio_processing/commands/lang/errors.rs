//! Source positions and language errors (REQ-056 §7).

use std::fmt;

/// A place in the source text: 1-based line and column (counted in
/// characters), and a length in characters. A span that crosses lines only
/// covers its first line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub len: u32,
}

impl Span {
    /// A span from the start of `self` to the end of `other`. If they are on
    /// different lines the result keeps `self` unchanged.
    pub fn to(self, other: Span) -> Span {
        if self.line != other.line {
            return self;
        }
        let end = other.end_col().max(self.end_col());
        Span {
            line: self.line,
            col: self.col,
            len: end - self.col,
        }
    }

    /// The column just past the last character.
    pub fn end_col(self) -> u32 {
        self.col + self.len
    }
}

/// Where a piece of text came from: `"<prompt>"` or a file name.
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

/// Every error and warning code of REQ-056 §7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    /// E101
    UnclosedString,
    /// E102
    BadNumber,
    // E103 (a clock-shaped name) is retired; the number isn't reused.
    /// E104
    UnknownChar,
    /// E105: `&` or `&&`, which `and` and `then` replaced.
    AmpRemoved,
    /// E201
    Unexpected,
    /// E202
    UnclosedBlock,
    /// E203
    PipeNoReceiver,
    /// E204
    WhenInBlock,
    /// E205
    EqNoValue,
    /// E206: `end` with no script to end, or without saying which.
    NoScript,
    /// E207: `--export` of a script that ends at once.
    ExportEmpty,
    /// E301
    UnknownName,
    /// E302
    KindClash,
    /// E303
    ReservedWord,
    /// E304
    AmbiguousPath,
    /// E401
    UnknownSetting,
    /// E402
    WrongType,
    /// E403
    OutOfRange,
    /// E404
    NotBuiltYet,
    /// E405
    MissingItem,
    /// E406
    StepOutsideCycle,
    /// E407
    InUse,
    /// E408
    ImportFailed,
    /// E409
    ClockOff,
    /// E410: a scheduled line creates or removes something it can't yet
    /// (`at`/`after` lines; buffers, tracks, effects removals).
    CannotWait,
    /// E412: a wave's `fm=` would make a loop of modulators.
    Cycle,
    /// E411: a statement typed now names an object that a waiting line will
    /// make (it does not exist yet).
    NotCreatedYet,
    /// E501
    UnknownClock,
    /// E502
    NothingToTrigger,
    /// E503
    MainNotRemovable,
    /// E504: a receiver's target isn't a voice (or, for `trig`, a group).
    BadReceiver,
    /// E601
    LimitReached,
    /// E602
    QueueFull,
    /// E701
    SourceNotFound,
    /// E702
    SourceCycle,
    /// E703
    SourceDepth,
    /// E704
    SourceWrite,
    /// E708: the folder a buffer's `to=` file goes in isn't there.
    BadSaveFolder,
    /// E709: the memory for a buffer could not be had.
    NoMemory,
    /// W101
    ChangesNothing,
    /// W102: removing a voice or group took it out of a seq's or packet's
    /// pipe (and removed the seq or packet if the pipe was left empty).
    ReceiverDropped,
    /// W103: removing something dropped a waiting line that named it.
    WaitingLineDropped,
    /// W104: a setting outside its range was clamped to it (`window=`).
    Clamped,
    /// W105: a `thenif` was dropped because a glide on its left side was cut
    /// short (or an object it names is gone).
    ChainCutShort,
    /// W106: `wait D and x` was read as `wait D then x`.
    WaitAnd,
    /// W107: a line of a running script no longer lowers (an object it names
    /// is gone), so it was skipped.
    ScriptLineSkipped,
    /// W108: a setting that only means something for a recorded source
    /// (`start`, `loop`, ...) on a voice that plays waves only.
    WavesOnly,
    /// W109: a table wave was cut from a buffer with nothing recorded, so it
    /// has no table yet.
    EmptySource,
}

impl Code {
    /// The printed form, e.g. `"E301"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::UnclosedString => "E101",
            Code::BadNumber => "E102",
            Code::UnknownChar => "E104",
            Code::AmpRemoved => "E105",
            Code::Unexpected => "E201",
            Code::UnclosedBlock => "E202",
            Code::PipeNoReceiver => "E203",
            Code::WhenInBlock => "E204",
            Code::EqNoValue => "E205",
            Code::NoScript => "E206",
            Code::ExportEmpty => "E207",
            Code::UnknownName => "E301",
            Code::KindClash => "E302",
            Code::ReservedWord => "E303",
            Code::AmbiguousPath => "E304",
            Code::UnknownSetting => "E401",
            Code::WrongType => "E402",
            Code::OutOfRange => "E403",
            Code::NotBuiltYet => "E404",
            Code::MissingItem => "E405",
            Code::StepOutsideCycle => "E406",
            Code::InUse => "E407",
            Code::ImportFailed => "E408",
            Code::ClockOff => "E409",
            Code::CannotWait => "E410",
            Code::NotCreatedYet => "E411",
            Code::Cycle => "E412",
            Code::UnknownClock => "E501",
            Code::NothingToTrigger => "E502",
            Code::MainNotRemovable => "E503",
            Code::BadReceiver => "E504",
            Code::LimitReached => "E601",
            Code::QueueFull => "E602",
            Code::SourceNotFound => "E701",
            Code::SourceCycle => "E702",
            Code::SourceDepth => "E703",
            Code::SourceWrite => "E704",
            Code::BadSaveFolder => "E708",
            Code::NoMemory => "E709",
            Code::ChangesNothing => "W101",
            Code::ReceiverDropped => "W102",
            Code::WaitingLineDropped => "W103",
            Code::Clamped => "W104",
            Code::ChainCutShort => "W105",
            Code::WaitAnd => "W106",
            Code::ScriptLineSkipped => "W107",
            Code::WavesOnly => "W108",
            Code::EmptySource => "W109",
        }
    }

    /// True for `W…` codes.
    pub fn is_warning(self) -> bool {
        matches!(
            self,
            Code::ChangesNothing
                | Code::ReceiverDropped
                | Code::WaitingLineDropped
                | Code::Clamped
                | Code::ChainCutShort
                | Code::WaitAnd
                | Code::ScriptLineSkipped
                | Code::WavesOnly
                | Code::EmptySource
        )
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One syntax (or later, semantic) error with its position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LangErr {
    pub code: Code,
    pub span: Span,
    pub message: String,
    /// A hint such as `did you mean 'hat'?`.
    pub suggestion: Option<String>,
    /// The file the error is in, when it isn't the prompt's text (scripts,
    /// LANGd).
    pub file: Option<String>,
}

impl LangErr {
    /// An error without a suggestion.
    pub fn new(code: Code, span: Span, message: impl Into<String>) -> LangErr {
        LangErr {
            code,
            span,
            message: message.into(),
            suggestion: None,
            file: None,
        }
    }

    /// Names the file the error is in, unless it already says.
    pub fn in_file(mut self, name: &str) -> LangErr {
        if self.file.is_none() {
            self.file = Some(name.to_string());
        }
        self
    }

    /// Adds a hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> LangErr {
        self.suggestion = Some(hint.into());
        self
    }

    /// Adds a `did you mean 'x'?` hint when `suggest` finds a candidate.
    pub fn with_suggestion<'a>(
        self,
        word: &str,
        candidates: impl IntoIterator<Item = &'a str>,
    ) -> LangErr {
        match suggest(word, candidates) {
            Some(c) => self.with_hint(format!("did you mean '{c}'?")),
            None => self,
        }
    }

    /// The error with its source name in front: `set.blast:12:7: E301 …`.
    /// An error that names its own file keeps it.
    pub fn with_source(&self, name: &str) -> String {
        match self.file {
            Some(_) => self.to_string(),
            None => format!("{name}:{self}"),
        }
    }
}

impl fmt::Display for LangErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{file}:")?;
        }
        // a span with no line (a file that can't be read) has no position
        if self.span.line > 0 {
            write!(f, "{}:{}:", self.span.line, self.span.col)?;
        }
        if self.file.is_some() || self.span.line > 0 {
            f.write_str(" ")?;
        }
        write!(f, "{} {}", self.code, self.message)?;
        if let Some(s) = &self.suggestion {
            write!(f, " ({s})")?;
        }
        Ok(())
    }
}

impl std::error::Error for LangErr {}

/// The candidate closest to `word` by edit distance (at most 2, and less
/// than the word's length so short words don't match everything). Ties go
/// to the earlier candidate.
pub fn suggest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let wlen = word.chars().count();
    let mut best: Option<(usize, &'a str)> = None;
    for c in candidates {
        let d = distance(word, c);
        if d <= 2 && d < wlen && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

/// Levenshtein distance over characters.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_picks_closest() {
        let settings = ["gain", "pan", "rate", "pitch"];
        assert_eq!(suggest("gian", settings), Some("gain"));
        assert_eq!(suggest("zzz", settings), None);
        assert_eq!(suggest("pa", settings), Some("pan"));
        // ties go to the earlier candidate
        assert_eq!(suggest("rat", ["rate", "gate"]), Some("rate"));
    }

    #[test]
    fn display_and_source_prefix() {
        let e = LangErr::new(
            Code::UnknownName,
            Span {
                line: 12,
                col: 7,
                len: 3,
            },
            "unknown name 'hta'",
        )
        .with_suggestion("hta", ["hat", "kick"]);
        assert_eq!(
            e.to_string(),
            "12:7: E301 unknown name 'hta' (did you mean 'hat'?)"
        );
        assert_eq!(
            e.with_source("set.blast"),
            "set.blast:12:7: E301 unknown name 'hta' (did you mean 'hat'?)"
        );
    }
}
