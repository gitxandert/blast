//! The syntax tree of REQ-056 §2. Every node carries its `Span`.

use super::errors::Span;
use crate::audio_processing::engine::FxType;

/// A whole script: one `Line` per non-empty line.
#[derive(Clone, Debug, PartialEq)]
pub struct Script {
    pub lines: Vec<Line>,
}

/// Parts joined by `then` and `thenif`, each part statements joined by
/// `and` (or a `wait`), and an optional trailing `at`/`after`.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Never empty. A line without `then`, `thenif` or `wait` has one `And`.
    pub parts: Vec<Part>,
    /// `joins[i]` sits between `parts[i]` and `parts[i + 1]`.
    pub joins: Vec<Join>,
    pub when: Option<When>,
    pub span: Span,
    /// The line as written (from its first token to its last), for
    /// messages that quote it.
    pub text: String,
}

impl Line {
    /// A line that is nothing but `wait D [by CLOCK]`: it moves a script's
    /// cursor.
    pub fn lone_wait(&self) -> Option<&Wait> {
        match (self.parts.as_slice(), &self.when) {
            ([Part::Wait(w)], None) => Some(w),
            _ => None,
        }
    }

    /// The statements of the first part; empty if it is a `wait`. For
    /// lines that can't have more than one part (those in a block).
    pub fn chain(&self) -> &[Stmt] {
        match self.parts.first() {
            Some(Part::And(stmts)) => stmts,
            _ => &[],
        }
    }

    /// Every statement of every part, left to right.
    pub fn stmts(&self) -> impl Iterator<Item = &Stmt> {
        self.parts.iter().flat_map(|p| match p {
            Part::And(stmts) => stmts.as_slice(),
            Part::Wait(_) => &[],
        })
    }

    /// `stmts`, to change them.
    pub fn stmts_mut(&mut self) -> impl Iterator<Item = &mut Stmt> {
        self.parts.iter_mut().flat_map(|p| match p {
            Part::And(stmts) => stmts.as_mut_slice(),
            Part::Wait(_) => &mut [],
        })
    }

    /// Whether the line is one plain `and` group: nothing to wait for.
    pub fn is_simple(&self) -> bool {
        self.joins.is_empty() && matches!(self.parts.as_slice(), [Part::And(_)])
    }
}

/// One side of a `then`: statements that happen together, or a pause.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// Statements joined by `and`.
    And(Vec<Stmt>),
    Wait(Wait),
}

/// How the part after a join is run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    /// `then`: once the left part is done, whatever happened to it.
    Then(Span),
    /// `thenif`: once the left part is done, if nothing cut it short.
    ThenIf(Span),
    /// `wait D and x`, which means `wait D then x` (W106).
    WaitAnd(Span),
}

/// `wait D [by CLOCK]`: a part that does nothing and is done after `D`.
#[derive(Clone, Debug, PartialEq)]
pub struct Wait {
    /// A `Value::Num` or `Value::Duration`.
    pub value: Value,
    pub by: Option<ClockRef>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Object(Object),
    Action(Action),
    Let(Let),
    Source(SourceStmt),
    Query(Query),
    End(EndStmt),
}

/// `[KIND] target {item} [block] [pipe]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub kind: Option<(Kind, Span)>,
    pub target: Target,
    pub items: Vec<Item>,
    pub block: Option<Block>,
    pub pipe: Option<Vec<Receiver>>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Path(Vec<Ident>),
    /// A quoted track name.
    Track(String, Span),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    From(String, Span),
    /// `from NAME`: a voice plays a buffer (RECb).
    FromName(Ident),
    On(ClockRef),
    In(Num),
    /// `name=value`, and an optional `over D` (a glide, REQ-074).
    Setting(Ident, Value, Option<Over>),
    Step(Step),
    Clock(ClockLit),
}

/// `over D`: how long a setting takes to get to its value. A bare number is
/// periods of the object; `b` counts ticks and the time units count time.
#[derive(Clone, Debug, PartialEq)]
pub struct Over {
    /// A `Value::Num` or `Value::Duration`.
    pub value: Value,
    /// From the `over` word to the end of the duration.
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub lines: Vec<Line>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Receiver {
    pub kind: (ReceiverKind, Span),
    pub target: Option<Vec<Ident>>,
    pub items: Vec<Item>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhenWord {
    At,
    After,
}

#[derive(Clone, Debug, PartialEq)]
pub struct When {
    pub word: WhenWord,
    pub n: Num,
    pub by: Option<ClockRef>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClockRef {
    Path(Vec<Ident>),
    Lit(ClockLit),
}

/// A clock written like a duration: `93b`, `500ms`, `24000f`, `($n * 2)b`.
#[derive(Clone, Debug, PartialEq)]
pub struct ClockLit {
    /// `None`: a bare `$var`; the unit comes from the variable.
    pub unit: Option<DurUnit>,
    pub n: Num,
    pub span: Span,
    /// `over D` after the rate of a `clock` statement: a glide to it.
    pub over: Option<Over>,
}

/// One rhythm position, 1-based as written (no conversion here).
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub at: Num,
    /// The end of an inclusive range `at..to`.
    pub to: Option<Num>,
    /// `%n`, percent.
    pub chance: Option<Num>,
    /// `~a`: early or late by up to `a` (overrides the seq's feel).
    pub jitter: Option<Num>,
    pub span: Span,
}

/// The unit of a pitch value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitchUnit {
    Semitones,
    Cents,
    Ratio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurUnit {
    Ticks,
    S,
    Ms,
    Us,
    Ns,
    Frames,
    /// A rate, not a length: `5hz`. Only `lfo freq=` takes one.
    Hz,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(Num),
    Duration(Num, DurUnit),
    /// Decibels (`-6db`); only the settings marked `db` take one.
    Db(Num),
    /// A pitch in semitones (`7st`), cents (`50ct`) or a frequency ratio
    /// (`1.5x`); only `pitch` takes one.
    Pitch(Num, PitchUnit),
    Word(Ident),
    /// Two or more words joined by touching `+` (`fm=a+b`); only `fm`
    /// takes one.
    Words(Vec<Ident>),
    /// A number with the text it was written as (`ratio=3/2`): made by the
    /// parser for `ratio` when the text is number literals and `+ - * / ( )`.
    Typed(Num, String),
    Str(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Num {
    Lit(f64, Span),
    Var(Ident),
    Expr(Box<Expr>, Span),
}

impl Num {
    /// Where the number was written.
    pub fn span(&self) -> Span {
        match self {
            Num::Lit(_, s) | Num::Expr(_, s) => *s,
            Num::Var(i) => i.span,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Var(Ident),
    Neg(Box<Expr>),
    Bin(Box<Expr>, BinOp, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Clock,
    Track,
    Voice,
    Group,
    Seq,
    Packet,
    Lfo,
    Env,
    /// An effect (FXR): each registry entry has its own keyword.
    Fx(FxType),
    Wave,
    /// Memory that voices play while it records (RECb).
    Buffer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverKind {
    Trig,
    Melo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionWord {
    Start,
    Stop,
    Pause,
    Resume,
    Remove,
    Ungroup,
    Rescan,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub word: (ActionWord, Span),
    pub targets: ActionTargets,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ActionTargets {
    All(Span),
    /// Empty only for `rescan`.
    List(Vec<Target>),
}

/// `let name = value`; the value is a number or a duration.
#[derive(Clone, Debug, PartialEq)]
pub struct Let {
    pub name: Ident,
    pub value: Value,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceStmt {
    pub path: SourcePath,
    /// `read`: print the file, don't run it.
    pub read: bool,
    /// `history`: not built yet (E404).
    pub history: bool,
    /// `stop`: cancels the running script of that name.
    pub stop: bool,
    pub span: Span,
}

/// `end`, `end NAME` or `end after D [by CLOCK]`: stops what the running
/// script started and cancels its remaining lines.
#[derive(Clone, Debug, PartialEq)]
pub struct EndStmt {
    /// `end NAME`: a script that is running, ended from outside it.
    pub name: Option<Ident>,
    /// `end after D`: ends once `D` has passed (a whole line of a script).
    pub after: Option<Wait>,
    pub span: Span,
}

/// A source file as written: a bare name (`phase`, `lib/kit`, `set-1`) or a
/// quoted path.
#[derive(Clone, Debug, PartialEq)]
pub enum SourcePath {
    Name(String, Span),
    Quoted(String, Span),
}

impl SourcePath {
    pub fn text(&self) -> &str {
        match self {
            SourcePath::Name(s, _) | SourcePath::Quoted(s, _) => s,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            SourcePath::Name(_, sp) | SourcePath::Quoted(_, sp) => *sp,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Query {
    /// `state [path] [to NAME]`.
    State(Option<Vec<Ident>>, Option<SourcePath>),
    Check(SourcePath),
    Help(Option<Ident>),
    Quit,
    Clear,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}
