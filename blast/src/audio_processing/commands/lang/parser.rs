//! Recursive-descent parser for the grammar of REQ-056 §2.

use super::ast::*;
use super::errors::{Code, LangErr, Source, Span};
use super::lexer::{KEYWORDS, Kw, Tok, Token, dur_unit, lex, pitch_unit};
use crate::audio_processing::engine::fx::{FxType, spec};

/// The outcome of a parse that found no errors.
#[derive(Clone, Debug, PartialEq)]
pub enum Parse {
    Complete(Script),
    /// The input ends inside a block or after `\`, `|`, `and`, `then`, `thenif` or `,`: more
    /// text could finish it.
    Incomplete,
}

/// Parses `src`. On any error, returns every error found, in source order;
/// the parser recovers at the next line end of the same block depth. Never
/// panics.
pub fn parse(src: Source) -> Result<Parse, Vec<LangErr>> {
    let lexed = lex(src.text);
    let mut p = Parser {
        toks: lexed.tokens,
        pos: 0,
        errs: lexed.errors,
        incomplete: lexed.continued_at_end,
        last_end: (0, 0),
        depth: 0,
        lines: src.text.split('\n').map(|l| l.chars().collect()).collect(),
    };
    let script = p.script();
    if !p.errs.is_empty() {
        let mut errs = p.errs;
        errs.sort_by_key(|e| (e.span.line, e.span.col));
        return Err(errs);
    }
    if p.incomplete {
        return Ok(Parse::Incomplete);
    }
    Ok(Parse::Complete(script))
}

/// A parse failed; the error (if any) is already recorded.
struct Fail;
type PResult<T> = Result<T, Fail>;

/// Deepest nesting of blocks and parentheses before the parser gives up
/// (keeps recursion bounded on hostile input).
const MAX_DEPTH: u32 = 64;

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    errs: Vec<LangErr>,
    incomplete: bool,
    /// (line, column) just past the last consumed token, for adjacency.
    last_end: (u32, u32),
    depth: u32,
    /// The source, by line, to rebuild a bare file name from its tokens.
    lines: Vec<Vec<char>>,
}

impl Parser {
    // ---- token access ----

    fn peek_at(&self, off: usize) -> &Token {
        // The token list always ends with Eof.
        let i = (self.pos + off).min(self.toks.len().saturating_sub(1));
        &self.toks[i]
    }

    fn peek(&self) -> &Tok {
        &self.peek_at(0).kind
    }

    fn span(&self) -> Span {
        self.peek_at(0).span
    }

    fn bump(&mut self) -> Token {
        let t = self.peek_at(0).clone();
        if t.kind != Tok::Eof {
            self.pos += 1;
        }
        self.last_end = (t.span.line, t.span.end_col());
        t
    }

    fn eat(&mut self, k: &Tok) -> bool {
        if self.peek() == k {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Is the token `off` ahead directly against the previous/next one?
    fn touching(&self, off: usize) -> bool {
        let prev_end = if off == 0 {
            self.last_end
        } else {
            let p = self.peek_at(off - 1).span;
            (p.line, p.end_col())
        };
        let s = self.peek_at(off).span;
        (s.line, s.col) == prev_end
    }

    fn at_eol(&self) -> bool {
        matches!(self.peek(), Tok::Newline | Tok::Semi | Tok::Eof)
    }

    fn skip_eols(&mut self) {
        while matches!(self.peek(), Tok::Newline | Tok::Semi) {
            self.bump();
        }
    }

    // ---- errors ----

    fn push(&mut self, e: LangErr) -> Fail {
        self.errs.push(e);
        Fail
    }

    /// Reports `code` at the current token, unless it is one the lexer
    /// already reported.
    fn fail_here(&mut self, code: Code, msg: String) -> Fail {
        if *self.peek() == Tok::Bad {
            return Fail;
        }
        let span = self.span();
        self.push(LangErr::new(code, span, msg))
    }

    fn unexpected(&mut self, wanted: &str) -> Fail {
        let found = self.peek().describe();
        if *self.peek() == Tok::Kw(Kw::Over) {
            let span = self.span();
            return self.push(
                LangErr::new(
                    Code::Unexpected,
                    span,
                    format!("expected {wanted}, found {found}"),
                )
                .with_hint("'over' follows a setting"),
            );
        }
        self.fail_here(
            Code::Unexpected,
            format!("expected {wanted}, found {found}"),
        )
    }

    // ---- script and lines ----

    fn script(&mut self) -> Script {
        let mut lines = Vec::new();
        loop {
            self.skip_eols();
            if *self.peek() == Tok::Eof {
                break;
            }
            let before = self.pos;
            match self.line(false) {
                Ok(l) => lines.push(l),
                Err(Fail) => self.recover(false),
            }
            if self.pos == before {
                self.bump();
            }
        }
        Script { lines }
    }

    /// Skips to the end of the line at the current block depth. Inside a
    /// block, stops at (without consuming) the closing `}`.
    fn recover(&mut self, in_block: bool) {
        let mut depth = 0u32;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::Newline | Tok::Semi if depth == 0 => return,
                Tok::LBrace => depth += 1,
                Tok::RBrace if depth == 0 => {
                    if in_block {
                        return;
                    }
                }
                Tok::RBrace => depth -= 1,
                _ => {}
            }
            self.bump();
        }
    }

    /// One line: a chain and, at top level, an optional `at`/`after`. Ends
    /// before the line end (or the `}` of an enclosing block).
    fn line(&mut self, in_block: bool) -> PResult<Line> {
        let start = self.span();
        let (parts, joins) = self.chain(in_block)?;
        let mut when = None;
        if matches!(self.peek(), Tok::Kw(Kw::At | Kw::After)) {
            if in_block {
                return Err(self.fail_here(
                    Code::WhenInBlock,
                    "'at' and 'after' go at the end of a whole line, not inside a block".into(),
                ));
            }
            when = Some(self.when()?);
        }
        if *self.peek() == Tok::Comma {
            // A trailing `,` continues the line; anywhere else it is stray.
            let comma = self.bump().span;
            self.continue_line()?;
            return Err(self.push(LangErr::new(Code::Unexpected, comma, "unexpected ','")));
        }
        if !(self.at_eol() || in_block && *self.peek() == Tok::RBrace) {
            return Err(self.unexpected("the end of the line"));
        }
        let end = when
            .as_ref()
            .map(|w| w.span)
            .or_else(|| parts.last().map(part_span))
            .unwrap_or(start);
        let text = self.text_from(start);
        Ok(Line {
            parts,
            joins,
            when,
            span: start.to(end),
            text,
        })
    }

    // the source from `start` to the last consumed token, line ends as
    // single spaces
    fn text_from(&self, start: Span) -> String {
        let (end_line, end_col) = self.last_end;
        let mut text = String::new();
        for no in start.line..=end_line {
            let Some(chars) = self.lines.get(no as usize - 1) else {
                break;
            };
            let from = if no == start.line {
                start.col as usize - 1
            } else {
                0
            };
            let to = if no == end_line {
                end_col as usize - 1
            } else {
                chars.len()
            };
            if !text.is_empty() {
                text.push(' ');
            }
            let part: String = chars
                .get(from..to.min(chars.len()))
                .unwrap_or(&[])
                .iter()
                .collect();
            text.push_str(part.trim());
        }
        text
    }

    // parts joined by `then` and `thenif`; each is `and`-joined statements
    // or a `wait`. A block's lines can't wait.
    fn chain(&mut self, in_block: bool) -> PResult<(Vec<Part>, Vec<Join>)> {
        let mut parts = vec![self.part(in_block)?];
        let mut joins = Vec::new();
        loop {
            let join = match self.peek() {
                Tok::Kw(Kw::Then) => Join::Then(self.span()),
                Tok::Kw(Kw::ThenIf) => Join::ThenIf(self.span()),
                // `wait D and x` is `wait D then x`, with a warning
                Tok::Kw(Kw::And) if matches!(parts.last(), Some(Part::Wait(_))) => {
                    Join::WaitAnd(self.span())
                }
                _ => break,
            };
            if in_block && !matches!(join, Join::WaitAnd(_)) {
                return Err(self.fail_here(
                    Code::WhenInBlock,
                    "'then' and 'thenif' go between whole statements of a line, not inside a block"
                        .into(),
                ));
            }
            self.bump();
            self.continue_line()?;
            joins.push(join);
            parts.push(self.part(in_block)?);
        }
        Ok((parts, joins))
    }

    // `stmt {and stmt}`, or a `wait`
    fn part(&mut self, in_block: bool) -> PResult<Part> {
        if *self.peek() == Tok::Kw(Kw::Wait) {
            if in_block {
                return Err(self.fail_here(
                    Code::WhenInBlock,
                    "'wait' goes in a whole line, not inside a block".into(),
                ));
            }
            return self.wait().map(Part::Wait);
        }
        let mut stmts = vec![self.stmt()?];
        while *self.peek() == Tok::Kw(Kw::And) {
            self.bump();
            self.continue_line()?;
            stmts.push(self.stmt()?);
        }
        Ok(Part::And(stmts))
    }

    // `end`, `end NAME`, `end after D [by CLOCK]`
    fn end_stmt(&mut self) -> PResult<EndStmt> {
        let at = self.bump().span;
        match self.peek() {
            Tok::Kw(Kw::After) => {
                let after = self.wait()?;
                Ok(EndStmt {
                    name: None,
                    span: at.to(after.span),
                    after: Some(after),
                })
            }
            Tok::Name(_) => {
                let name = self.name()?;
                Ok(EndStmt {
                    span: at.to(name.span),
                    name: Some(name),
                    after: None,
                })
            }
            _ => Ok(EndStmt {
                name: None,
                after: None,
                span: at,
            }),
        }
    }

    // `wait D [by CLOCK]`; also the `after D [by CLOCK]` of `end after`
    fn wait(&mut self) -> PResult<Wait> {
        let at = self.bump().span;
        if !matches!(
            self.peek(),
            Tok::Num(_) | Tok::Var(_) | Tok::LParen | Tok::Dur(..)
        ) {
            return Err(self.unexpected("a duration after 'wait' (4, 2b, 250ms)"));
        }
        let value = self.value(at)?;
        let mut end = match &value {
            Value::Num(n) | Value::Duration(n, _) => n.span(),
            _ => at,
        };
        let mut by = None;
        if self.eat(&Tok::Kw(Kw::By)) {
            let c = self.clockref()?;
            end = clockref_span(&c);
            by = Some(c);
        }
        Ok(Wait {
            value,
            by,
            span: at.to(end),
        })
    }

    /// After `and`, `then`, `thenif`, `|` or `,` the statement goes on, over line ends. End of
    /// input there means the text is incomplete.
    fn continue_line(&mut self) -> PResult<()> {
        while *self.peek() == Tok::Newline {
            self.bump();
        }
        if *self.peek() == Tok::Eof {
            self.incomplete = true;
            return Err(Fail);
        }
        Ok(())
    }

    fn when(&mut self) -> PResult<When> {
        let t = self.bump();
        let word = if t.kind == Tok::Kw(Kw::At) {
            WhenWord::At
        } else {
            WhenWord::After
        };
        let n = self.number()?;
        let mut end = n.span();
        let mut by = None;
        if self.eat(&Tok::Kw(Kw::By)) {
            let c = self.clockref()?;
            end = clockref_span(&c);
            by = Some(c);
        }
        Ok(When {
            word,
            n,
            by,
            span: t.span.to(end),
        })
    }

    // ---- statements ----

    fn stmt(&mut self) -> PResult<Stmt> {
        match self.peek().clone() {
            Tok::Act(_) => self.action().map(Stmt::Action),
            Tok::Kw(Kw::Let) => self.let_stmt().map(Stmt::Let),
            Tok::Kw(Kw::Source) => {
                let t = self.bump();
                let path = self.source_path("source")?;
                let mut span = t.span.to(path.span());
                let (mut read, mut history, mut stop) = (false, false, false);
                // `read` (`r`) and `history` (`h`) are plain names, in any order
                loop {
                    match self.peek() {
                        Tok::Name(n) if matches!(n.as_str(), "read" | "r") => read = true,
                        Tok::Name(n) if matches!(n.as_str(), "history" | "h") => history = true,
                        Tok::Act(ActionWord::Stop) => stop = true,
                        _ => break,
                    }
                    span = span.to(self.bump().span);
                }
                Ok(Stmt::Source(SourceStmt {
                    path,
                    read,
                    history,
                    stop,
                    span,
                }))
            }
            Tok::Kw(Kw::End) => self.end_stmt().map(Stmt::End),
            Tok::Kw(Kw::State | Kw::Check | Kw::Help | Kw::Quit | Kw::Clear) => {
                self.query().map(Stmt::Query)
            }
            Tok::Kind(_) | Tok::Name(_) | Tok::Str(_) => self.object().map(Stmt::Object),
            _ => Err(self.unexpected("a statement")),
        }
    }

    /// Is the next token the word `to`, with a file name after it on the
    /// same line? (`to` is a plain name: `state to` alone is a voice.)
    fn is_to(&self) -> bool {
        matches!(self.peek(), Tok::Name(n) if n == "to")
            && (matches!(self.peek_at(1).kind, Tok::Str(_)) || Self::is_word(&self.peek_at(1).kind))
    }

    fn is_word(t: &Tok) -> bool {
        matches!(t, Tok::Name(_) | Tok::Num(_) | Tok::Dur(..)) || t.keyword_text().is_some()
    }

    /// A file name: a quoted string, or words joined with no spaces by `/`,
    /// `.` or `-` (`phase`, `lib/kit`, `set-1`), taken as written.
    fn source_path(&mut self, after: &str) -> PResult<SourcePath> {
        if let Tok::Str(s) = self.peek().clone() {
            let span = self.bump().span;
            return Ok(SourcePath::Quoted(s, span));
        }
        if !Self::is_word(self.peek()) {
            return Err(self.unexpected(&format!(
                "a file name (a bare name or a quoted path) after {after}"
            )));
        }
        let first = self.bump().span;
        let mut span = first;
        loop {
            let sep = matches!(self.peek(), Tok::Slash | Tok::Dot | Tok::Minus);
            let more =
                sep && self.touching(0) && Self::is_word(&self.peek_at(1).kind) && self.touching(1);
            if !more {
                break;
            }
            self.bump();
            span = span.to(self.bump().span);
        }
        let text: String = self
            .lines
            .get(first.line as usize - 1)
            .map(|l| {
                let from = first.col as usize - 1;
                l.iter()
                    .skip(from)
                    .take(span.end_col() as usize - first.col as usize)
            })
            .map_or_else(String::new, |it| it.collect());
        Ok(SourcePath::Name(text, span))
    }

    fn query(&mut self) -> PResult<Query> {
        let t = self.bump();
        match t.kind {
            Tok::Kw(Kw::Quit) => Ok(Query::Quit),
            Tok::Kw(Kw::Clear) => Ok(Query::Clear),
            Tok::Kw(Kw::State) => {
                let path = if matches!(self.peek(), Tok::Name(_)) && !self.is_to() {
                    Some(self.path()?)
                } else {
                    None
                };
                let to = if self.is_to() {
                    self.bump();
                    Some(self.source_path("'to'")?)
                } else {
                    None
                };
                Ok(Query::State(path, to))
            }
            Tok::Kw(Kw::Check) => Ok(Query::Check(self.source_path("check")?)),
            _ => {
                // help [WORD]: any word, including a reserved one (`help voice`).
                let tok = self.peek().clone();
                let word = match &tok {
                    Tok::Name(n) => Some(n.clone()),
                    t => t.keyword_text().map(str::to_string),
                };
                match word {
                    Some(name) => {
                        let span = self.bump().span;
                        Ok(Query::Help(Some(Ident { name, span })))
                    }
                    None => Ok(Query::Help(None)),
                }
            }
        }
    }

    fn action(&mut self) -> PResult<Action> {
        let t = self.bump();
        let Tok::Act(word) = t.kind else {
            return Err(Fail);
        };
        if word == ActionWord::Rescan {
            return Ok(Action {
                word: (word, t.span),
                targets: ActionTargets::List(Vec::new()),
                span: t.span,
            });
        }
        if *self.peek() == Tok::Kw(Kw::All) {
            let a = self.bump();
            return Ok(Action {
                word: (word, t.span),
                targets: ActionTargets::All(a.span),
                span: t.span.to(a.span),
            });
        }
        let mut targets = Vec::new();
        let mut end = t.span;
        while matches!(self.peek(), Tok::Name(_) | Tok::Str(_)) {
            let tg = self.target()?;
            end = target_span(&tg);
            targets.push(tg);
        }
        if targets.is_empty() {
            return Err(self.unexpected("a name or 'all'"));
        }
        Ok(Action {
            word: (word, t.span),
            targets: ActionTargets::List(targets),
            span: t.span.to(end),
        })
    }

    fn let_stmt(&mut self) -> PResult<Let> {
        let t = self.bump();
        let name = self.name()?;
        let eq = self.span();
        if !self.eat(&Tok::Eq) {
            return Err(self.unexpected("'=' after the name"));
        }
        let value = self.value(eq)?;
        if matches!(value, Value::Word(_) | Value::Words(_) | Value::Str(_)) {
            return Err(self.push(LangErr::new(
                Code::WrongType,
                name.span,
                "a 'let' value is a number, a clock or a duration",
            )));
        }
        let end = value_span(&value).unwrap_or(eq);
        Ok(Let {
            name,
            value,
            span: t.span.to(end),
        })
    }

    fn object(&mut self) -> PResult<Object> {
        let start = self.span();
        let mut kind = None;
        if let Tok::Kind(k) = *self.peek() {
            kind = Some((k, self.bump().span));
        }
        let target = self.target()?;
        let mut end = target_span(&target);
        let mut items = Vec::new();
        let kind_word = kind.map(|k| k.0);
        while let Some(item) = self.item(kind_word)? {
            end = item_span(&item);
            items.push(item);
        }
        let mut block = None;
        if *self.peek() == Tok::LBrace {
            let b = self.block()?;
            end = b.span;
            block = Some(b);
        }
        let mut pipe = None;
        if *self.peek() == Tok::Pipe {
            let rs = self.pipe()?;
            if let Some(r) = rs.last() {
                end = receiver_span(r);
            }
            pipe = Some(rs);
        }
        Ok(Object {
            kind,
            target,
            items,
            block,
            pipe,
            span: start.to(end),
        })
    }

    fn block(&mut self) -> PResult<Block> {
        let open = self.bump();
        if self.depth >= MAX_DEPTH {
            return Err(self.fail_here(Code::Unexpected, "blocks nested too deeply".into()));
        }
        self.depth += 1;
        let mut lines = Vec::new();
        let result = loop {
            self.skip_eols();
            match self.peek() {
                Tok::Eof => {
                    // With other errors already found, the text can't be
                    // completed into something valid by typing more.
                    if self.errs.is_empty() {
                        self.incomplete = true;
                    } else {
                        let e = LangErr::new(Code::UnclosedBlock, open.span, "unclosed '{'")
                            .with_hint("add a closing '}'");
                        self.errs.push(e);
                    }
                    break Err(Fail);
                }
                Tok::RBrace => {
                    let close = self.bump();
                    break Ok(Block {
                        lines,
                        span: open.span.to(close.span),
                    });
                }
                _ => {}
            }
            let before = self.pos;
            match self.line(true) {
                Ok(l) => lines.push(l),
                Err(Fail) => self.recover(true),
            }
            if self.pos == before {
                self.bump();
            }
        };
        self.depth -= 1;
        result
    }

    fn pipe(&mut self) -> PResult<Vec<Receiver>> {
        let bar = self.bump();
        self.continue_line()?;
        let mut rs = vec![self.receiver(bar.span)?];
        while *self.peek() == Tok::Comma {
            self.bump();
            self.continue_line()?;
            rs.push(self.receiver(bar.span)?);
        }
        Ok(rs)
    }

    fn receiver(&mut self, bar: Span) -> PResult<Receiver> {
        let Tok::Recv(kind) = *self.peek() else {
            if *self.peek() == Tok::Bad {
                return Err(Fail);
            }
            let found = self.peek().describe();
            let mut e = LangErr::new(
                Code::PipeNoReceiver,
                bar,
                format!("'|' needs a receiver ('trig' or 'melo'), found {found}"),
            );
            if let Tok::Name(n) = self.peek() {
                e = e.with_suggestion(n, ["trig", "melo"]);
            }
            return Err(self.push(e));
        };
        let kind = (kind, self.bump().span);
        let mut target = None;
        if matches!(self.peek(), Tok::Name(_)) && *self.peek_at(1).kind_ref() != Tok::Eq {
            target = Some(self.path()?);
        }
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Tok::Dur(..) => items.push(Item::Clock(self.clock_lit()?)),
                Tok::LParen if self.paren_unit_ahead() => {
                    items.push(Item::Clock(self.clock_lit()?))
                }
                Tok::Num(_) | Tok::Var(_) | Tok::LParen | Tok::Minus => {
                    if *self.peek() == Tok::Minus && !self.minus_literal() {
                        break;
                    }
                    items.push(Item::Step(self.step()?));
                }
                _ => break,
            }
        }
        Ok(Receiver {
            kind,
            target,
            items,
        })
    }

    // ---- items ----

    /// The next item of an object statement, or `None` at its end.
    fn item(&mut self, kind: Option<Kind>) -> PResult<Option<Item>> {
        // `word =` is a setting, even for reserved words (`start=0.2`).
        let word_text = match self.peek() {
            Tok::Name(n) => Some(n.clone()),
            t => t.keyword_text().map(str::to_string),
        };
        if let Some(name) = word_text
            && *self.peek_at(1).kind_ref() == Tok::Eq
        {
            let id = Ident {
                name,
                span: self.bump().span,
            };
            let eq = self.bump().span;
            let start = self.span();
            let mut value = self.value(eq)?;
            // `ratio=3/2` is kept as typed (state prints it back)
            if id.name == "ratio"
                && let Value::Num(n @ (Num::Lit(..) | Num::Expr(..))) = &value
            {
                let text: String = self
                    .text_from(start)
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                if !text.is_empty() && text.chars().all(|c| "0123456789.+-*/()".contains(c)) {
                    value = Value::Typed(n.clone(), text);
                }
            }
            let over = self.over()?;
            return Ok(Some(Item::Setting(id, value, over)));
        }
        match self.peek().clone() {
            Tok::Kw(Kw::From) => {
                self.bump();
                match self.peek().clone() {
                    Tok::Str(s) => {
                        let sp = self.bump().span;
                        Ok(Some(Item::From(s, sp)))
                    }
                    // a buffer, by its bare name (RECb)
                    Tok::Name(n) => {
                        let span = self.bump().span;
                        Ok(Some(Item::FromName(Ident { name: n, span })))
                    }
                    _ => Err(self.unexpected("a quoted track name or a buffer after 'from'")),
                }
            }
            Tok::Kw(Kw::On) => {
                self.bump();
                Ok(Some(Item::On(self.clockref()?)))
            }
            Tok::Kw(Kw::In) => {
                self.bump();
                Ok(Some(Item::In(self.number()?)))
            }
            Tok::Dur(..) => Ok(Some(Item::Clock(self.clock_stmt_lit(kind)?))),
            // a `clock` statement's rate may be a variable or `(expr)UNIT`
            Tok::Var(_) | Tok::LParen if kind == Some(Kind::Clock) => {
                Ok(Some(Item::Clock(self.clock_stmt_lit(kind)?)))
            }
            Tok::LParen if self.paren_unit_ahead() => {
                Ok(Some(Item::Clock(self.clock_stmt_lit(kind)?)))
            }
            Tok::Num(_) | Tok::Var(_) | Tok::LParen => Ok(Some(Item::Step(self.step()?))),
            Tok::Minus if self.minus_literal() => Ok(Some(Item::Step(self.step()?))),
            _ => Ok(None),
        }
    }

    /// The `over D` after a setting's value, if there is one.
    fn over(&mut self) -> PResult<Option<Over>> {
        if *self.peek() != Tok::Kw(Kw::Over) {
            return Ok(None);
        }
        let at = self.bump().span;
        if !matches!(
            self.peek(),
            Tok::Num(_) | Tok::Var(_) | Tok::LParen | Tok::Dur(..)
        ) {
            return Err(self.unexpected("a duration after 'over' (1, 2b, 250ms)"));
        }
        let value = self.value(at)?;
        let end = match &value {
            Value::Num(n) | Value::Duration(n, _) => n.span(),
            _ => at,
        };
        Ok(Some(Over {
            value,
            span: at.to(end),
        }))
    }

    /// Is the `-` at the cursor directly in front of a number literal?
    fn minus_literal(&self) -> bool {
        matches!(self.peek_at(1).kind, Tok::Num(_)) && self.touching(1)
    }

    fn step(&mut self) -> PResult<Step> {
        let at = self.number()?;
        let start = at.span();
        let mut end = start;
        let mut to = None;
        let mut chance = None;
        let mut jitter = None;
        if *self.peek() == Tok::DotDot && self.touching(0) {
            self.bump();
            let n = self.adjacent_number("a number after '..'")?;
            end = n.span();
            to = Some(n);
        }
        loop {
            if !self.touching(0) {
                break;
            }
            match self.peek() {
                Tok::Percent => {
                    let m = self.bump().span;
                    if chance.is_some() {
                        return Err(self.push(LangErr::new(
                            Code::Unexpected,
                            m,
                            "a step has one '%' chance",
                        )));
                    }
                    let n = self.adjacent_number("a percentage after '%'")?;
                    end = n.span();
                    chance = Some(n);
                }
                Tok::Tilde => {
                    let m = self.bump().span;
                    if jitter.is_some() {
                        return Err(self.push(LangErr::new(
                            Code::Unexpected,
                            m,
                            "a step has one '~' jitter",
                        )));
                    }
                    let a = self.adjacent_number("a number after '~'")?;
                    end = a.span();
                    if *self.peek() == Tok::DotDot && self.touching(0) {
                        let dots = self.bump().span;
                        // consume the second number so recovery is clean
                        let _ = self.adjacent_number("a number after '..'");
                        return Err(self.push(
                            LangErr::new(Code::Unexpected, dots, "a '~' mark has no range")
                                .with_hint(
                                    "a one-sided feel is a seq setting: late=0.04 (or ~0.04 for early or late)",
                                ),
                        ));
                    }
                    jitter = Some(a);
                }
                _ => break,
            }
        }
        Ok(Step {
            at,
            to,
            chance,
            jitter,
            span: start.to(end),
        })
    }

    /// A number that must touch the previous token (no space in a step).
    fn adjacent_number(&mut self, wanted: &str) -> PResult<Num> {
        if !self.touching(0) {
            return Err(self.unexpected(wanted));
        }
        self.number()
    }

    /// Is the `(` at the cursor closed by a `)` directly followed by a
    /// duration unit word (`($n * 2)b`)?
    fn paren_unit_ahead(&self) -> bool {
        let mut depth = 0u32;
        let mut i = 0;
        loop {
            match self.peek_at(i).kind {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return self.unit_at(i + 1);
                    }
                }
                Tok::Eof | Tok::Newline | Tok::Semi => return false,
                _ => {}
            }
            i += 1;
        }
    }

    /// Is the token `off` ahead a unit word touching the one before it?
    fn unit_at(&self, off: usize) -> bool {
        matches!(&self.peek_at(off).kind, Tok::Name(n) if dur_unit(n).is_some())
            && self.touching(off)
    }

    /// A clock: `93b`, `500ms`, `($n * 2)b`, or a bare `$var` (the unit comes
    /// from the variable).
    fn clock_lit(&mut self) -> PResult<ClockLit> {
        match self.peek().clone() {
            Tok::Dur(n, unit) => {
                let span = self.bump().span;
                Ok(ClockLit {
                    unit: Some(unit),
                    n: Num::Lit(n, span),
                    span,
                    over: None,
                })
            }
            Tok::Var(_) => {
                let n = self.number()?;
                Ok(ClockLit {
                    unit: None,
                    span: n.span(),
                    n,
                    over: None,
                })
            }
            Tok::LParen => {
                let n = self.number()?;
                let Some(unit) = self.unit_word() else {
                    return Err(self.unexpected("a unit (b, ms, s, f) right after ')'"));
                };
                let span = n.span().to(unit.1);
                Ok(ClockLit {
                    unit: Some(unit.0),
                    n,
                    span,
                    over: None,
                })
            }
            _ => Err(self.unexpected("a clock (93b, 500ms, 24000f)")),
        }
    }

    /// A clock rate in an object statement. In a `clock` statement it may be
    /// followed by `over D` (`clock main 140b over 8`, REQ-074): the one place
    /// `over` follows something that isn't a setting.
    fn clock_stmt_lit(&mut self, kind: Option<Kind>) -> PResult<ClockLit> {
        let mut lit = self.clock_lit()?;
        if kind == Some(Kind::Clock) {
            lit.over = self.over()?;
        }
        Ok(lit)
    }

    /// Consumes a unit word touching the previous token.
    fn unit_word(&mut self) -> Option<(DurUnit, Span)> {
        if !self.unit_at(0) {
            return None;
        }
        let t = self.bump();
        match t.kind {
            Tok::Name(n) => dur_unit(&n).map(|u| (u, t.span)),
            _ => None,
        }
    }

    /// A pitch unit word (`st`, `ct`, `x`) touching the token before it.
    fn pitch_unit_word(&mut self) -> Option<PitchUnit> {
        let unit = match self.peek() {
            Tok::Name(n) if self.touching(0) => pitch_unit(n)?,
            _ => return None,
        };
        self.bump();
        Some(unit)
    }

    fn clockref(&mut self) -> PResult<ClockRef> {
        if matches!(self.peek(), Tok::Dur(..) | Tok::Var(_) | Tok::LParen) {
            return self.clock_lit().map(ClockRef::Lit);
        }
        self.path().map(ClockRef::Path)
    }

    /// The value after `=`. `eq` is the span of the `=` for E205.
    fn value(&mut self, eq: Span) -> PResult<Value> {
        match self.peek().clone() {
            Tok::Num(_) | Tok::Var(_) | Tok::LParen => {
                // Without parentheses a `let` value may be a full expression.
                let n = self.expr_number()?;
                // `($bpm * 2)b`: a parenthesised number with a unit
                if matches!(n, Num::Expr(..))
                    && let Some((unit, _)) = self.unit_word()
                {
                    return Ok(Value::Duration(n, unit));
                }
                if matches!(n, Num::Expr(..))
                    && let Some(unit) = self.pitch_unit_word()
                {
                    return Ok(Value::Pitch(n, unit));
                }
                Ok(Value::Num(n))
            }
            Tok::Minus if matches!(self.peek_at(1).kind, Tok::Db(_)) && self.touching(1) => {
                let m = self.bump().span;
                let Tok::Db(n) = self.peek().clone() else {
                    return Err(Fail);
                };
                let sp = self.bump().span;
                Ok(Value::Db(Num::Lit(-n, m.to(sp))))
            }
            Tok::Minus if matches!(self.peek_at(1).kind, Tok::Pitch(..)) && self.touching(1) => {
                let m = self.bump().span;
                let Tok::Pitch(n, unit) = self.peek().clone() else {
                    return Err(Fail);
                };
                let sp = self.bump().span;
                Ok(Value::Pitch(Num::Lit(-n, m.to(sp)), unit))
            }
            Tok::Minus if self.minus_literal() => self.number().map(Value::Num),
            Tok::Minus => self.expr_number().map(Value::Num),
            Tok::Dur(n, unit) => {
                let sp = self.bump().span;
                Ok(Value::Duration(Num::Lit(n, sp), unit))
            }
            Tok::Db(n) => {
                let sp = self.bump().span;
                Ok(Value::Db(Num::Lit(n, sp)))
            }
            Tok::Pitch(n, unit) => {
                let sp = self.bump().span;
                Ok(Value::Pitch(Num::Lit(n, sp), unit))
            }
            Tok::Name(n) => {
                let mut span = self.bump().span;
                let mut name = n;
                // `lo.cutoff`, `kick.lo.cutoff`: one word with the dots in it
                while *self.peek() == Tok::Dot
                    && self.touching(0)
                    && matches!(self.peek_at(1).kind, Tok::Name(_))
                    && self.touching(1)
                {
                    self.bump();
                    let Tok::Name(next) = self.peek().clone() else {
                        break;
                    };
                    span = span.to(self.bump().span);
                    name.push('.');
                    name.push_str(&next);
                }
                let first = Ident { name, span };
                // `a+b`: words joined by touching `+` (no spaces)
                let mut more = Vec::new();
                while *self.peek() == Tok::Plus
                    && self.touching(0)
                    && matches!(self.peek_at(1).kind, Tok::Name(_))
                    && self.touching(1)
                {
                    self.bump();
                    let Tok::Name(next) = self.peek().clone() else {
                        break;
                    };
                    let span = self.bump().span;
                    more.push(Ident { name: next, span });
                }
                if more.is_empty() {
                    return Ok(Value::Word(first));
                }
                more.insert(0, first);
                Ok(Value::Words(more))
            }
            // `to=start`: an action word is a plain word here
            Tok::Act(w) => {
                let span = self.bump().span;
                let name = Tok::Act(w).keyword_text().unwrap_or("start").to_string();
                Ok(Value::Word(Ident { name, span }))
            }
            // `choirmode=seq`: the seq kind keyword is also a value word
            Tok::Kind(Kind::Seq) => {
                let span = self.bump().span;
                Ok(Value::Word(Ident {
                    name: "seq".into(),
                    span,
                }))
            }
            Tok::Kw(k @ (Kw::Yes | Kw::No)) => {
                let span = self.bump().span;
                let name = if k == Kw::Yes { "yes" } else { "no" };
                Ok(Value::Word(Ident {
                    name: name.into(),
                    span,
                }))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Value::Str(s))
            }
            Tok::Newline
            | Tok::Semi
            | Tok::Eof
            | Tok::RBrace
            | Tok::Pipe
            | Tok::Comma
            | Tok::Kw(Kw::At | Kw::After | Kw::And | Kw::Then | Kw::ThenIf) => Err(self.push(
                LangErr::new(Code::EqNoValue, eq, "'=' needs a value")
                    .with_hint("write a value after the '='"),
            )),
            _ => Err(self.unexpected("a value after '='")),
        }
    }

    // ---- names and paths ----

    /// A NAME. Reserved words (E303) get their own error.
    fn name(&mut self) -> PResult<Ident> {
        match self.peek().clone() {
            Tok::Name(n) => {
                let span = self.bump().span;
                Ok(Ident { name: n, span })
            }
            t => match t.keyword_text() {
                Some(w) => Err(self.fail_here(
                    Code::ReservedWord,
                    format!("'{w}' is a reserved word and can't be a name"),
                )),
                None => Err(self.unexpected("a name")),
            },
        }
    }

    fn path(&mut self) -> PResult<Vec<Ident>> {
        let mut p = vec![self.name()?];
        while *self.peek() == Tok::Dot {
            self.bump();
            p.push(self.name()?);
        }
        Ok(p)
    }

    fn target(&mut self) -> PResult<Target> {
        if let Tok::Str(s) = self.peek().clone() {
            let span = self.bump().span;
            return Ok(Target::Track(s, span));
        }
        self.path().map(Target::Path)
    }

    // ---- numbers ----

    /// NUMBER, VAR or `( expr )`; a `-` touching a literal is part of it.
    fn number(&mut self) -> PResult<Num> {
        match self.peek().clone() {
            Tok::Num(n) => {
                let span = self.bump().span;
                Ok(Num::Lit(n, span))
            }
            Tok::Var(v) => {
                let span = self.bump().span;
                Ok(Num::Var(Ident { name: v, span }))
            }
            Tok::Minus if self.minus_literal() => {
                let m = self.bump().span;
                let Tok::Num(n) = self.peek().clone() else {
                    return Err(Fail);
                };
                let sp = self.bump().span;
                Ok(Num::Lit(-n, m.to(sp)))
            }
            Tok::LParen => {
                let open = self.bump().span;
                let e = self.expr()?;
                let close = self.span();
                if !self.eat(&Tok::RParen) {
                    return Err(self.unexpected("')'"));
                }
                Ok(Num::Expr(Box::new(e), open.to(close)))
            }
            _ => Err(self.unexpected("a number")),
        }
    }

    /// A number or, for `let`, an unparenthesised expression like `$a / 2`.
    fn expr_number(&mut self) -> PResult<Num> {
        let start = self.span();
        if *self.peek() == Tok::LParen {
            // Parenthesised, but may continue: `(1 + 2) * 3`.
            let first = self.number()?;
            if !matches!(self.peek(), Tok::Plus | Tok::Minus | Tok::Star | Tok::Slash) {
                return Ok(first);
            }
            // Re-parse from the start as a full expression.
            let Num::Expr(inner, _) = first else {
                return Err(Fail);
            };
            let e = self.expr_rest(*inner)?;
            return Ok(fold(e, start.to_last(self)));
        }
        let e = self.expr()?;
        Ok(fold(e, start.to_last(self)))
    }

    fn expr(&mut self) -> PResult<Expr> {
        let first = self.term()?;
        self.expr_rest(first)
    }

    /// Continues an expression whose first operand is `lhs`.
    fn expr_rest(&mut self, lhs: Expr) -> PResult<Expr> {
        let mut lhs = self.term_rest(lhs)?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.term()?;
            lhs = Expr::Bin(Box::new(lhs), op, Box::new(rhs));
        }
    }

    fn term(&mut self) -> PResult<Expr> {
        let first = self.unary()?;
        self.term_rest(first)
    }

    fn term_rest(&mut self, mut lhs: Expr) -> PResult<Expr> {
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.unary()?;
            lhs = Expr::Bin(Box::new(lhs), op, Box::new(rhs));
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        if self.eat(&Tok::Minus) {
            return Ok(Expr::Neg(Box::new(self.atom()?)));
        }
        self.atom()
    }

    fn atom(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Tok::Num(n) => {
                self.bump();
                Ok(Expr::Num(n))
            }
            Tok::Var(v) => {
                let span = self.bump().span;
                Ok(Expr::Var(Ident { name: v, span }))
            }
            Tok::LParen => {
                if self.depth >= MAX_DEPTH {
                    return Err(
                        self.fail_here(Code::Unexpected, "expression nested too deeply".into())
                    );
                }
                self.bump();
                self.depth += 1;
                let e = self.expr();
                self.depth -= 1;
                let e = e?;
                if !self.eat(&Tok::RParen) {
                    return Err(self.unexpected("')'"));
                }
                Ok(e)
            }
            _ => Err(self.unexpected("a number")),
        }
    }
}

// ---- small helpers ----

trait ToLast {
    fn to_last(self, p: &Parser) -> Span;
}

impl ToLast for Span {
    /// The span from `self` to the last consumed token.
    fn to_last(self, p: &Parser) -> Span {
        let (line, end) = p.last_end;
        if line != self.line {
            return self;
        }
        Span {
            line,
            col: self.col,
            len: end.saturating_sub(self.col),
        }
    }
}

trait KindRef {
    fn kind_ref(&self) -> &Tok;
}

impl KindRef for Token {
    fn kind_ref(&self) -> &Tok {
        &self.kind
    }
}

/// A lone literal or variable stays simple; anything else is an `Expr`.
fn fold(e: Expr, span: Span) -> Num {
    match e {
        Expr::Num(n) => Num::Lit(n, span),
        Expr::Neg(inner) => match *inner {
            Expr::Num(n) => Num::Lit(-n, span),
            other => Num::Expr(Box::new(Expr::Neg(Box::new(other))), span),
        },
        Expr::Var(id) => Num::Var(id),
        other => Num::Expr(Box::new(other), span),
    }
}

fn target_span(t: &Target) -> Span {
    match t {
        Target::Track(_, s) => *s,
        Target::Path(p) => match (p.first(), p.last()) {
            (Some(a), Some(b)) => a.span.to(b.span),
            _ => Span::default(),
        },
    }
}

fn clockref_span(c: &ClockRef) -> Span {
    match c {
        ClockRef::Lit(l) => l.span,
        ClockRef::Path(p) => match (p.first(), p.last()) {
            (Some(a), Some(b)) => a.span.to(b.span),
            _ => Span::default(),
        },
    }
}

fn item_span(i: &Item) -> Span {
    match i {
        Item::From(_, s) => *s,
        Item::FromName(id) => id.span,
        Item::On(c) => clockref_span(c),
        Item::In(n) => n.span(),
        Item::Setting(id, v, over) => match over {
            Some(o) => id.span.to(o.span),
            None => value_span(v).map_or(id.span, |e| id.span.to(e)),
        },
        Item::Step(s) => s.span,
        Item::Clock(c) => c.span,
    }
}

fn value_span(v: &Value) -> Option<Span> {
    match v {
        Value::Num(n)
        | Value::Typed(n, _)
        | Value::Duration(n, _)
        | Value::Db(n)
        | Value::Pitch(n, _) => Some(n.span()),
        Value::Word(i) => Some(i.span),
        Value::Words(w) => w.first().map(|f| f.span.to(w[w.len() - 1].span)),
        Value::Str(_) => None,
    }
}

fn receiver_span(r: &Receiver) -> Span {
    let mut end = r.kind.1;
    if let Some(t) = r.target.as_ref().and_then(|p| p.last()) {
        end = t.span;
    }
    if let Some(i) = r.items.last() {
        end = item_span(i);
    }
    r.kind.1.to(end)
}

fn part_span(p: &Part) -> Span {
    match p {
        Part::And(stmts) => stmts.last().map_or(Span::default(), stmt_span),
        Part::Wait(w) => w.span,
    }
}

fn stmt_span(s: &Stmt) -> Span {
    match s {
        Stmt::Object(o) => o.span,
        Stmt::Action(a) => a.span,
        Stmt::Let(l) => l.span,
        Stmt::Source(s) => s.span,
        Stmt::Query(_) => Span::default(),
        Stmt::End(e) => e.span,
    }
}

/// All reserved words, for tests and callers that want the list.
pub fn reserved_words() -> impl Iterator<Item = &'static str> {
    KEYWORDS
        .iter()
        .map(|(w, _)| *w)
        .chain(FxType::ALL.iter().map(|t| spec(*t).word))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn src(text: &str) -> Source<'_> {
        Source {
            name: "<test>",
            text,
        }
    }

    fn script(text: &str) -> Script {
        match parse(src(text)) {
            Ok(Parse::Complete(s)) => s,
            other => panic!("{text}: {other:?}"),
        }
    }

    fn errs(text: &str) -> Vec<(Code, u32, u32)> {
        match parse(src(text)) {
            Err(es) => es
                .iter()
                .map(|e| (e.code, e.span.line, e.span.col))
                .collect(),
            other => panic!("{text}: expected errors, got {other:?}"),
        }
    }

    fn only_stmt(text: &str) -> Stmt {
        let mut s = script(text);
        assert_eq!(s.lines.len(), 1, "{text}");
        let l = s.lines.remove(0);
        assert_eq!(l.chain().len(), 1, "{text}");
        l.chain()[0].clone()
    }

    fn object(text: &str) -> Object {
        match only_stmt(text) {
            Stmt::Object(o) => o,
            other => panic!("{other:?}"),
        }
    }

    pub(crate) const PIECES: &str = r#"
# 1. Four on the floor at 124.
clock main 124b
voice kick from "kick"
kick { seq beat 1 2 3 4 in 4 | trig }
start kick

# 2. Offbeat hats; the last one only sometimes.
voice hat from "hat" gain=0.4
hat { seq off 1.5 2.5 3.5 4.5%40 in 4 | trig }

# 3. Kick and hat in a group, started together.
group drums { kick; hat }
start drums

# 4. Hats at 93 against the kick at 124.
hat on 93b

# 5. The kick's rhythm also plays the bass, on low notes.
voice bass from "bass"
kick.beat | trig, melo bass 36 36 43 41

# 6. Two fills into the snare as one bundle.
voice snare from "snare"
packet fills {
  seq a 4 4.5 in 4
  seq b 2.75 4.25 in 8
} | trig snare

# 7. Everything up to 128, and the hat back onto main.
clock main 128b and hat on main

# 8. An organ from three sines (waves are future work).
voice organ {
  wave s1 shape=sine freq=220
  wave s2 shape=sine freq=440
  wave s3 shape=sine freq=880
}
organ.s2 freq=660

# 9. Humanise the snare: lazy on the backbeat.
snare { seq back 2 4~0.05 late=0.03 in 4 | trig }

# 10. The lead on main's next 16; slow main on its next bar.
start lead at 16 by main
clock main 120b at 4
"#;

    pub(crate) const STATE: &str = r#"
clock main 124b
group drums {
  voice kick from "kick"
  voice hat from "hat" on 93b gain=0.4
}
voice bass from "bass"
kick { seq beat 1 2 3 4 in 4 | trig, melo bass 36 36 43 41 }
hat { seq off 1.5 2.5 3.5 4.5%40 in 4 | trig }
# playing: drums bass
"#;

    #[test]
    fn parses_the_ten_pieces() {
        let s = script(PIECES);
        // 1:3+1, 2:2, 3:2, 4:1, 5:2, 6:2, 7:1, 8:2, 9:1, 10:2
        assert_eq!(s.lines.len(), 4 + 2 + 2 + 1 + 2 + 2 + 1 + 2 + 1 + 2);
        // piece 7: one line, two statements
        let l = s.lines.iter().find(|l| l.chain().len() == 2).unwrap();
        assert!(
            matches!(&l.chain()[0], Stmt::Object(o) if o.kind.as_ref().unwrap().0 == Kind::Clock)
        );
        // piece 8: `wave` is a kind word
        let organ = s
            .lines
            .iter()
            .find_map(|l| match &l.chain()[0] {
                Stmt::Object(o) if o.block.as_ref().is_some_and(|b| b.lines.len() == 3) => Some(o),
                _ => None,
            })
            .unwrap();
        let waves = &organ.block.as_ref().unwrap().lines;
        assert!(waves.iter().all(|l| matches!(&l.chain()[0],
            Stmt::Object(o) if o.kind.as_ref().unwrap().0 == Kind::Wave && o.items.len() == 2)));
        // piece 10: `at 16 by main` and `at 4`
        let whens: Vec<_> = s.lines.iter().filter_map(|l| l.when.as_ref()).collect();
        assert_eq!(whens.len(), 2);
        assert_eq!(whens[0].word, WhenWord::At);
        assert!(whens[0].by.is_some());
        assert!(whens[1].by.is_none());
        // piece 6: a packet with two seqs and a pipe
        let packet = s
            .lines
            .iter()
            .find_map(|l| match &l.chain()[0] {
                Stmt::Object(o) if o.kind.as_ref().is_some_and(|k| k.0 == Kind::Packet) => Some(o),
                _ => None,
            })
            .unwrap();
        assert_eq!(packet.block.as_ref().unwrap().lines.len(), 2);
        assert_eq!(packet.pipe.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn parses_state_example() {
        let s = script(STATE);
        assert_eq!(s.lines.len(), 5);
        let Stmt::Object(g) = &s.lines[1].chain()[0] else {
            panic!()
        };
        assert_eq!(g.block.as_ref().unwrap().lines.len(), 2);
        let Stmt::Object(k) = &s.lines[3].chain()[0] else {
            panic!()
        };
        let Stmt::Object(seq) = &k.block.as_ref().unwrap().lines[0].chain()[0] else {
            panic!()
        };
        let pipe = seq.pipe.as_ref().unwrap();
        assert_eq!(pipe.len(), 2);
        assert_eq!(pipe[0].kind.0, ReceiverKind::Trig);
        assert_eq!(pipe[1].kind.0, ReceiverKind::Melo);
    }

    #[test]
    fn steps_and_marks() {
        let o = object("seq a 1 2.5%40 3~0.05 4~0.03 1..16%70~0.02 $x%50 ($x+1)");
        let steps: Vec<&Step> = o
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Step(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(steps.len(), 7);
        let lit = |n: &Num| match n {
            Num::Lit(v, _) => *v,
            other => panic!("{other:?}"),
        };
        assert_eq!(lit(&steps[0].at), 1.0);
        assert!(steps[0].chance.is_none() && steps[0].jitter.is_none());
        assert_eq!(lit(&steps[1].at), 2.5);
        assert_eq!(lit(steps[1].chance.as_ref().unwrap()), 40.0);
        assert_eq!(lit(steps[2].jitter.as_ref().unwrap()), 0.05);
        assert_eq!(lit(steps[3].jitter.as_ref().unwrap()), 0.03);
        assert_eq!(lit(steps[4].to.as_ref().unwrap()), 16.0);
        assert_eq!(lit(steps[4].chance.as_ref().unwrap()), 70.0);
        assert_eq!(lit(steps[4].jitter.as_ref().unwrap()), 0.02);
        assert!(matches!(&steps[5].at, Num::Var(v) if v.name == "x"));
        assert_eq!(lit(steps[5].chance.as_ref().unwrap()), 50.0);
        assert!(matches!(&steps[6].at, Num::Expr(..)));
        // whitespace ends a step: a detached mark is an error
        assert_eq!(errs("seq a 1 %50"), [(Code::Unexpected, 1, 9)]);
    }

    #[test]
    fn pipes_and_receivers() {
        let o = object("kick.beat | trig, melo bass 36 36 43 41");
        let Target::Path(p) = &o.target else { panic!() };
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].name.as_str(), p[1].name.as_str()), ("kick", "beat"));
        let rs = o.pipe.unwrap();
        assert_eq!(rs.len(), 2);
        assert!(rs[0].target.is_none() && rs[0].items.is_empty());
        assert_eq!(rs[1].target.as_ref().unwrap()[0].name, "bass");
        assert_eq!(rs[1].items.len(), 4);
    }

    #[test]
    fn chain_and_when() {
        let s = script("clock main 128b and hat on main at 4 by main");
        let l = &s.lines[0];
        assert_eq!(l.chain().len(), 2);
        let w = l.when.as_ref().unwrap();
        assert_eq!(w.word, WhenWord::At);
        assert!(matches!(&w.by, Some(ClockRef::Path(p)) if p[0].name == "main"));
        let s = script("start kick after 2");
        assert_eq!(s.lines[0].when.as_ref().unwrap().word, WhenWord::After);
    }

    #[test]
    fn old_clock_spellings_are_names() {
        // `b93` is an ordinary name now
        let o = object("voice b93 from \"x\"");
        assert!(matches!(&o.target, Target::Path(p) if p[0].name == "b93"));
    }

    #[test]
    fn clock_literals_parse() {
        let clock_of = |t: &str| match object(t).items.into_iter().next() {
            Some(Item::Clock(c)) => c,
            other => panic!("{t}: {other:?}"),
        };
        assert_eq!(clock_of("clock a 93b").unit, Some(DurUnit::Ticks));
        assert_eq!(clock_of("clock a 500ms").unit, Some(DurUnit::Ms));
        assert_eq!(clock_of("clock a 0.5s").unit, Some(DurUnit::S));
        assert_eq!(clock_of("clock a 24000f").unit, Some(DurUnit::Frames));
        assert_eq!(clock_of("clock a ($n * 2)b").unit, Some(DurUnit::Ticks));
        assert_eq!(clock_of("clock a $c").unit, None);
    }

    #[test]
    fn values_lets_and_queries() {
        let o =
            object("voice v from \"x\" fade=2b loop=forward flip=yes pan=-1 attack=5ms gain=$g");
        assert_eq!(o.items.len(), 7);
        assert!(matches!(
            &o.items[1],
            Item::Setting(_, Value::Duration(_, DurUnit::Ticks), _)
        ));
        assert!(matches!(&o.items[2], Item::Setting(_, Value::Word(w), _) if w.name == "forward"));
        assert!(matches!(&o.items[3], Item::Setting(_, Value::Word(w), _) if w.name == "yes"));
        assert!(
            matches!(&o.items[4], Item::Setting(_, Value::Num(Num::Lit(v, _)), _) if *v == -1.0)
        );
        assert!(matches!(
            &o.items[5],
            Item::Setting(_, Value::Duration(_, DurUnit::Ms), _)
        ));
        // reserved words as setting names
        let o = object("kick start=0.2 end=0.8");
        assert_eq!(o.items.len(), 2);
        assert!(matches!(only_stmt("let bpm = 96"), Stmt::Let(l) if l.name.name == "bpm"));
        assert!(
            matches!(only_stmt("let half = ($bpm / 2)"), Stmt::Let(l) if matches!(l.value, Value::Num(Num::Expr(..))))
        );
        assert!(
            matches!(only_stmt("let half = $bpm / 2"), Stmt::Let(l) if matches!(l.value, Value::Num(Num::Expr(..))))
        );
        assert!(
            matches!(only_stmt("let c = 124b"), Stmt::Let(l) if matches!(l.value, Value::Duration(..)))
        );
        assert!(matches!(only_stmt("clock main ($bpm)b"), Stmt::Object(_)));
        assert!(matches!(only_stmt("clock main ($bpm*2)b"), Stmt::Object(_)));
        assert!(
            matches!(only_stmt("source \"lib/drums.blast\""), Stmt::Source(s) if matches!(&s.path, SourcePath::Quoted(p, _) if p == "lib/drums.blast"))
        );
        assert!(matches!(
            only_stmt("state"),
            Stmt::Query(Query::State(None, None))
        ));
        assert!(
            matches!(only_stmt("state drums.hat"), Stmt::Query(Query::State(Some(p), None)) if p.len() == 2)
        );
        assert!(matches!(
            only_stmt("check \"f.blast\""),
            Stmt::Query(Query::Check(_))
        ));
        assert!(
            matches!(only_stmt("help voice"), Stmt::Query(Query::Help(Some(w))) if w.name == "voice")
        );
        assert!(matches!(only_stmt("quit"), Stmt::Query(Query::Quit)));
        assert!(matches!(only_stmt("exit"), Stmt::Query(Query::Quit)));
        let name = |text: &str| match only_stmt(text) {
            Stmt::Source(s) => (s.path.text().to_string(), s.read, s.history),
            other => panic!("{other:?}"),
        };
        assert_eq!(name("source phase"), ("phase".into(), false, false));
        assert_eq!(name("source lib/kit"), ("lib/kit".into(), false, false));
        assert_eq!(name("source set-1 r"), ("set-1".into(), true, false));
        assert_eq!(name("source a.b h read"), ("a.b".into(), true, true));
        assert!(matches!(
            only_stmt("state kit to lib/set-1"),
            Stmt::Query(Query::State(Some(p), Some(SourcePath::Name(n, _)))) if p.len() == 1 && n == "lib/set-1"
        ));
        assert!(matches!(
            only_stmt("state to \"x y\""),
            Stmt::Query(Query::State(None, Some(SourcePath::Quoted(..))))
        ));
        // `to` alone is still a name
        assert!(matches!(
            only_stmt("state to"),
            Stmt::Query(Query::State(Some(_), None))
        ));
        assert!(matches!(only_stmt("rescan"), Stmt::Action(a) if a.word.0 == ActionWord::Rescan));
        assert!(
            matches!(only_stmt("stop all"), Stmt::Action(a) if matches!(a.targets, ActionTargets::All(_)))
        );
        assert!(matches!(only_stmt("remove \"bass\" kick"), Stmt::Action(a)
            if matches!(&a.targets, ActionTargets::List(t) if t.len() == 2)));
        assert!(matches!(
            only_stmt("track \"bass\" from \"~/s/bass.wav\""),
            Stmt::Object(_)
        ));
    }

    #[test]
    fn incomplete_input() {
        for t in [
            "group drums {",
            "kick.beat |",
            "a and",
            "a then",
            "a thenif",
            "x \\",
            "a,",
            "kick.beat | trig,",
        ] {
            assert_eq!(parse(src(t)), Ok(Parse::Incomplete), "{t}");
        }
        assert_eq!(errs("}"), [(Code::Unexpected, 1, 1)]);
    }

    #[test]
    fn errors_have_codes_and_positions() {
        let table: &[(&str, Code, u32, u32)] = &[
            ("voice x from \"abc", Code::UnclosedString, 1, 14),
            ("seq a .5", Code::BadNumber, 1, 7),
            ("kick { seq a 1 at 4 }", Code::WhenInBlock, 1, 16),
            ("kick.beat |\n}", Code::PipeNoReceiver, 1, 11),
            ("kick gain=", Code::EqNoValue, 1, 10),
            ("let all = 1", Code::ReservedWord, 1, 5),
            ("kick $", Code::UnknownChar, 1, 6),
            ("kick )", Code::Unexpected, 1, 6),
            ("voice from", Code::ReservedWord, 1, 7),
            ("start", Code::Unexpected, 1, 6),
            ("kick | trg", Code::PipeNoReceiver, 1, 6),
        ];
        for (text, code, line, col) in table {
            let e = errs(text);
            assert_eq!(e[0], (*code, *line, *col), "{text}: {e:?}");
            assert_eq!(e.len(), 1, "{text}: {e:?}");
        }
        // suggestion on a mistyped receiver
        let Err(es) = parse(src("kick | trg")) else {
            panic!()
        };
        assert_eq!(es[0].suggestion.as_deref(), Some("did you mean 'trig'?"));
    }

    #[test]
    fn unclosed_block_with_other_errors() {
        let e = errs("group d {\n voice from\n");
        assert!(e.contains(&(Code::ReservedWord, 2, 8)), "{e:?}");
        assert!(e.contains(&(Code::UnclosedBlock, 1, 9)), "{e:?}");
    }

    #[test]
    fn reports_every_error() {
        let e = errs("voice from \"x\"\nkick gain=\nlet all = 1\nclock main 124b\n");
        assert_eq!(
            e,
            [
                (Code::ReservedWord, 1, 7),
                (Code::EqNoValue, 2, 10),
                (Code::ReservedWord, 3, 5)
            ]
        );
        // errors inside blocks are found too, and the block stays balanced
        let e = errs("group d {\n kick $\n hat )\n}\nvoice from\n");
        assert_eq!(e.len(), 3, "{e:?}");
    }

    #[test]
    fn never_panics() {
        let mut texts = vec![PIECES.to_string(), STATE.to_string()];
        texts.push(
            "((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((("
                .into(),
        );
        texts.push(
            "{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{"
                .into(),
        );
        for t in &texts {
            let bytes = t.as_bytes();
            for i in 0..=bytes.len() {
                if let Ok(prefix) = std::str::from_utf8(&bytes[..i]) {
                    let _ = parse(src(prefix));
                }
                let mut del = bytes.to_vec();
                if i < del.len() {
                    del.remove(i);
                    if let Ok(s) = String::from_utf8(del) {
                        let _ = parse(src(&s));
                    }
                }
            }
        }
        // odd characters
        for t in [
            "\u{0}",
            "é",
            "\"é",
            "\\",
            "$",
            "b(",
            "1..",
            "~~",
            "%%",
            "a.b.",
            "kick |, ,",
        ] {
            let _ = parse(src(t));
        }
    }

    #[test]
    fn dotted_setting_values_are_one_word() {
        let o = object("lfo w to=lo.cutoff depth=1.5 freq=5.5hz");
        assert!(
            matches!(&o.items[0], Item::Setting(_, Value::Word(w), _) if w.name == "lo.cutoff")
        );
        assert!(
            matches!(&o.items[1], Item::Setting(_, Value::Num(Num::Lit(v, _)), _) if *v == 1.5)
        );
        assert!(matches!(
            &o.items[2],
            Item::Setting(_, Value::Duration(_, DurUnit::Hz), _)
        ));
        let o = object("lfo w to=kick.lo.cutoff");
        assert!(
            matches!(&o.items[0], Item::Setting(_, Value::Word(w), _) if w.name == "kick.lo.cutoff")
        );
        // a space or a missing name ends the word
        assert!(!errs("lfo w to=lo. cutoff").is_empty());
        assert!(!errs("lfo w to=lo .cutoff").is_empty());
        // an action word is a plain word after `=`
        let o = object("lfo w to=start");
        assert!(matches!(&o.items[0], Item::Setting(_, Value::Word(w), _) if w.name == "start"));
    }

    #[test]
    fn the_pieces_have_no_reserved_collisions() {
        assert!(reserved_words().any(|w| w == "after"));
        for w in ["and", "then", "thenif", "wait"] {
            assert!(reserved_words().any(|r| r == w), "{w}");
        }
    }

    #[test]
    fn and_binds_tighter_than_then() {
        let s = script("a gain=1 and b gain=2 then c gain=3 thenif d gain=4 and e gain=5");
        let l = &s.lines[0];
        let sizes: Vec<usize> = l
            .parts
            .iter()
            .map(|p| match p {
                Part::And(stmts) => stmts.len(),
                Part::Wait(_) => 0,
            })
            .collect();
        assert_eq!(sizes, [2, 1, 2]);
        assert!(matches!(
            l.joins.as_slice(),
            [Join::Then(_), Join::ThenIf(_)]
        ));
        // the line goes on over a line end after the operator
        let s = script("a gain=1 then\n b gain=2");
        assert_eq!(s.lines[0].parts.len(), 2);
        assert_eq!(parse(src("a then")), Ok(Parse::Incomplete));
        assert_eq!(parse(src("a thenif")), Ok(Parse::Incomplete));
    }

    #[test]
    fn wait_is_a_part() {
        let s = script("wait 4b by main then a gain=1 at 8");
        let l = &s.lines[0];
        let Part::Wait(w) = &l.parts[0] else {
            panic!("a wait")
        };
        assert!(matches!(w.value, Value::Duration(_, DurUnit::Ticks)));
        assert!(w.by.is_some());
        assert!(l.when.is_some());
        let s = script("wait 500ms and a gain=1");
        assert!(matches!(s.lines[0].joins.as_slice(), [Join::WaitAnd(_)]));
        // `and` doesn't join a wait to a wait, and a wait can't follow `and`
        assert!(!errs("a gain=1 and wait 4").is_empty());
        assert!(!errs("wait").is_empty());
    }

    #[test]
    fn blocks_cannot_wait() {
        assert_eq!(errs("kick { a then b }")[0].0, Code::WhenInBlock);
        assert_eq!(errs("kick { wait 1 }")[0].0, Code::WhenInBlock);
    }
}
