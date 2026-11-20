//! Tokens of REQ-056 §1.

use super::ast::{ActionWord, DurUnit, Kind, PitchUnit, ReceiverKind};
use super::errors::{Code, LangErr, Span};
use crate::audio_processing::engine::fx::{from_word, spec};

/// Reserved words that are neither kinds, receivers nor actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kw {
    From,
    On,
    In,
    At,
    After,
    By,
    Over,
    And,
    Then,
    ThenIf,
    Wait,
    End,
    Let,
    Source,
    State,
    Check,
    Help,
    Quit,
    Clear,
    All,
    Yes,
    No,
}

/// What a token is.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Name(String),
    Str(String),
    Num(f64),
    /// A number with a unit suffix: `2b`, `5ms`. Also a clock: `93b`, `24000f`.
    Dur(f64, DurUnit),
    /// A number in decibels: `-6db` is a `Minus` and `Db(6.0)`. Only `gain` takes one.
    Db(f64),
    /// A pitch: `7st` (semitones), `50c` (cents), `1.5x` (a ratio). Only `pitch` takes one.
    Pitch(f64, PitchUnit),
    /// `$name` (without the `$`).
    Var(String),
    Kind(Kind),
    Recv(ReceiverKind),
    Act(ActionWord),
    Kw(Kw),
    LBrace,
    RBrace,
    Pipe,
    Comma,
    Semi,
    Eq,
    DotDot,
    Dot,
    Percent,
    Tilde,
    LParen,
    RParen,
    Plus,
    Minus,
    Star,
    Slash,
    Newline,
    Eof,
    /// A token the lexer already reported; the parser fails on it silently.
    Bad,
}

/// A token and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: Tok,
    pub span: Span,
}

/// Every reserved word (REQ-056 §1).
pub const KEYWORDS: &[(&str, Tok)] = &[
    ("clock", Tok::Kind(Kind::Clock)),
    ("track", Tok::Kind(Kind::Track)),
    ("voice", Tok::Kind(Kind::Voice)),
    ("group", Tok::Kind(Kind::Group)),
    ("seq", Tok::Kind(Kind::Seq)),
    ("packet", Tok::Kind(Kind::Packet)),
    ("lfo", Tok::Kind(Kind::Lfo)),
    ("env", Tok::Kind(Kind::Env)),
    ("wave", Tok::Kind(Kind::Wave)),
    ("buffer", Tok::Kind(Kind::Buffer)),
    ("from", Tok::Kw(Kw::From)),
    ("on", Tok::Kw(Kw::On)),
    ("in", Tok::Kw(Kw::In)),
    ("at", Tok::Kw(Kw::At)),
    ("after", Tok::Kw(Kw::After)),
    ("by", Tok::Kw(Kw::By)),
    ("over", Tok::Kw(Kw::Over)),
    ("and", Tok::Kw(Kw::And)),
    ("then", Tok::Kw(Kw::Then)),
    ("thenif", Tok::Kw(Kw::ThenIf)),
    ("wait", Tok::Kw(Kw::Wait)),
    ("end", Tok::Kw(Kw::End)),
    ("trig", Tok::Recv(ReceiverKind::Trig)),
    ("melo", Tok::Recv(ReceiverKind::Melo)),
    ("start", Tok::Act(ActionWord::Start)),
    ("stop", Tok::Act(ActionWord::Stop)),
    ("pause", Tok::Act(ActionWord::Pause)),
    ("resume", Tok::Act(ActionWord::Resume)),
    ("remove", Tok::Act(ActionWord::Remove)),
    ("ungroup", Tok::Act(ActionWord::Ungroup)),
    ("rescan", Tok::Act(ActionWord::Rescan)),
    ("let", Tok::Kw(Kw::Let)),
    ("source", Tok::Kw(Kw::Source)),
    ("state", Tok::Kw(Kw::State)),
    ("check", Tok::Kw(Kw::Check)),
    ("help", Tok::Kw(Kw::Help)),
    ("quit", Tok::Kw(Kw::Quit)),
    ("exit", Tok::Kw(Kw::Quit)),
    ("clear", Tok::Kw(Kw::Clear)),
    ("all", Tok::Kw(Kw::All)),
    ("yes", Tok::Kw(Kw::Yes)),
    ("no", Tok::Kw(Kw::No)),
];

/// The effect words (FXR) follow the other keywords: they come from the registry.
fn keyword(word: &str) -> Option<Tok> {
    KEYWORDS
        .iter()
        .find(|(w, _)| *w == word)
        .map(|(_, t)| t.clone())
        .or_else(|| from_word(word).map(|t| Tok::Kind(Kind::Fx(t))))
}

impl Tok {
    /// The source word of a reserved-word token.
    pub fn keyword_text(&self) -> Option<&'static str> {
        if let Tok::Kind(Kind::Fx(t)) = self {
            return Some(spec(*t).word);
        }
        KEYWORDS.iter().find(|(_, t)| t == self).map(|(w, _)| *w)
    }

    /// A short description for error messages.
    pub fn describe(&self) -> String {
        if let Some(w) = self.keyword_text() {
            return format!("'{w}'");
        }
        match self {
            Tok::Name(n) => format!("'{n}'"),
            Tok::Str(_) => "a string".into(),
            Tok::Num(n) => format!("'{n}'"),
            Tok::Dur(..) => "a duration".into(),
            Tok::Db(..) => "a decibel value".into(),
            Tok::Pitch(..) => "a pitch".into(),
            Tok::Var(v) => format!("'${v}'"),
            Tok::LBrace => "'{'".into(),
            Tok::RBrace => "'}'".into(),
            Tok::Pipe => "'|'".into(),
            Tok::Comma => "','".into(),
            Tok::Semi => "';'".into(),
            Tok::Eq => "'='".into(),
            Tok::DotDot => "'..'".into(),
            Tok::Dot => "'.'".into(),
            Tok::Percent => "'%'".into(),
            Tok::Tilde => "'~'".into(),
            Tok::LParen => "'('".into(),
            Tok::RParen => "')'".into(),
            Tok::Plus => "'+'".into(),
            Tok::Minus => "'-'".into(),
            Tok::Star => "'*'".into(),
            Tok::Slash => "'/'".into(),
            Tok::Newline | Tok::Eof => "end of line".into(),
            _ => "something unreadable".into(),
        }
    }
}

/// The tokens of a text, the errors found while lexing, and whether the text
/// ended on a `\` continuation.
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<LangErr>,
    pub continued_at_end: bool,
}

/// Splits `text` into tokens. Never fails: bad input becomes an error plus a
/// `Tok::Bad`. The last token is always `Eof`.
pub fn lex(text: &str) -> Lexed {
    let mut lx = Lexer {
        chars: text.chars().collect(),
        pos: 0,
        line: 1,
        col: 1,
        tokens: Vec::new(),
        errors: Vec::new(),
        continued_at_end: false,
    };
    lx.run();
    Lexed {
        tokens: lx.tokens,
        errors: lx.errors,
        continued_at_end: lx.continued_at_end,
    }
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
    tokens: Vec<Token>,
    errors: Vec<LangErr>,
    continued_at_end: bool,
}

/// The pitch unit a word spells (`st`, `c`, `x`).
pub fn pitch_unit(s: &str) -> Option<PitchUnit> {
    match s {
        "st" => Some(PitchUnit::Semitones),
        "c" => Some(PitchUnit::Cents),
        "x" => Some(PitchUnit::Ratio),
        _ => None,
    }
}

/// The duration unit a word spells (`b`, `s`, `ms`, `us`, `ns`, `f`, `hz`).
pub fn dur_unit(s: &str) -> Option<DurUnit> {
    match s {
        "b" => Some(DurUnit::Ticks),
        "s" => Some(DurUnit::S),
        "ms" => Some(DurUnit::Ms),
        "us" => Some(DurUnit::Us),
        "ns" => Some(DurUnit::Ns),
        "f" => Some(DurUnit::Frames),
        "hz" => Some(DurUnit::Hz),
        _ => None,
    }
}

impl Lexer {
    fn peek(&self, off: usize) -> Option<char> {
        self.chars.get(self.pos + off).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek(0)?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn span_from(&self, line: u32, col: u32) -> Span {
        Span {
            line,
            col,
            len: if self.line == line {
                self.col.saturating_sub(col)
            } else {
                1
            },
        }
    }

    fn push(&mut self, kind: Tok, line: u32, col: u32) {
        let span = self.span_from(line, col);
        self.tokens.push(Token { kind, span });
    }

    fn error(&mut self, code: Code, line: u32, col: u32, msg: String, hint: Option<&str>) {
        let mut e = LangErr::new(code, self.span_from(line, col), msg);
        if let Some(h) = hint {
            e = e.with_hint(h);
        }
        self.errors.push(e);
        self.push(Tok::Bad, line, col);
    }

    fn run(&mut self) {
        while let Some(c) = self.peek(0) {
            let (line, col) = (self.line, self.col);
            match c {
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '\n' => {
                    self.bump();
                    self.tokens.push(Token {
                        kind: Tok::Newline,
                        span: Span { line, col, len: 1 },
                    });
                }
                '#' => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                '\\' => self.continuation(line, col),
                '"' => self.string(line, col),
                '0'..='9' => self.number(line, col),
                '.' => self.dot(line, col),
                '$' => self.var(line, col),
                'a'..='z' | 'A'..='Z' | '_' => self.word(line, col),
                '&' => self.amp(line, col),
                _ => self.punct(c, line, col),
            }
        }
        let span = Span {
            line: self.line,
            col: self.col,
            len: 0,
        };
        self.tokens.push(Token {
            kind: Tok::Eof,
            span,
        });
    }

    /// `&` and `&&` were the old chain operators: now `and` and `then`.
    fn amp(&mut self, line: u32, col: u32) {
        self.bump();
        let (msg, hint) = if self.peek(0) == Some('&') {
            self.bump();
            ("'&&' is now 'then'", "write 'then'")
        } else {
            ("'&' is now 'and'", "write 'and'")
        };
        self.error(Code::AmpRemoved, line, col, msg.into(), Some(hint));
    }

    fn punct(&mut self, c: char, line: u32, col: u32) {
        let kind = match c {
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            '|' => Tok::Pipe,
            ',' => Tok::Comma,
            ';' => Tok::Semi,
            '=' => Tok::Eq,
            '%' => Tok::Percent,
            '~' => Tok::Tilde,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            _ => {
                self.bump();
                self.error(
                    Code::UnknownChar,
                    line,
                    col,
                    format!("unknown character '{c}'"),
                    None,
                );
                return;
            }
        };
        self.bump();
        self.push(kind, line, col);
    }

    /// `\` joins the next line to this one. After it only blanks may follow.
    fn continuation(&mut self, line: u32, col: u32) {
        self.bump();
        while matches!(self.peek(0), Some(' ' | '\t' | '\r')) {
            self.bump();
        }
        match self.peek(0) {
            Some('\n') => {
                self.bump();
            }
            None => self.continued_at_end = true,
            Some(_) => self.error(
                Code::UnknownChar,
                line,
                col,
                "'\\' must end a line".into(),
                None,
            ),
        }
    }

    fn string(&mut self, line: u32, col: u32) {
        self.bump();
        let mut s = String::new();
        loop {
            match self.peek(0) {
                None | Some('\n') => {
                    self.error(
                        Code::UnclosedString,
                        line,
                        col,
                        "unclosed string".into(),
                        Some("add a closing '\"'"),
                    );
                    return;
                }
                Some('"') => {
                    self.bump();
                    self.push(Tok::Str(s), line, col);
                    return;
                }
                Some('\\') => {
                    self.bump();
                    match self.peek(0) {
                        Some(c @ ('"' | '\\')) => {
                            self.bump();
                            s.push(c);
                        }
                        // Any other backslash stays as written (paths).
                        _ => s.push('\\'),
                    }
                }
                Some(c) => {
                    self.bump();
                    s.push(c);
                }
            }
        }
    }

    fn digits(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek(0).filter(char::is_ascii_digit) {
            self.bump();
            s.push(c);
        }
        s
    }

    /// Digits with an optional `.digits`. Returns `None` after reporting a
    /// trailing dot (`1.`).
    fn plain_number(&mut self, line: u32, col: u32) -> Option<f64> {
        let mut s = self.digits();
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            s.push('.');
            s.push_str(&self.digits());
        } else if self.peek(0) == Some('.') && self.peek(1) != Some('.') {
            self.bump();
            self.error(
                Code::BadNumber,
                line,
                col,
                format!("bad number '{s}.'"),
                Some("a number needs digits after the '.'"),
            );
            return None;
        }
        s.parse().ok()
    }

    fn number(&mut self, line: u32, col: u32) {
        let Some(n) = self.plain_number(line, col) else {
            return;
        };
        if !self
            .peek(0)
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.push(Tok::Num(n), line, col);
            return;
        }
        let mut suffix = String::new();
        while let Some(c) = self
            .peek(0)
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        {
            self.bump();
            suffix.push(c);
        }
        if suffix == "db" {
            self.push(Tok::Db(n), line, col);
            return;
        }
        if let Some(u) = pitch_unit(&suffix) {
            self.push(Tok::Pitch(n, u), line, col);
            return;
        }
        match dur_unit(&suffix) {
            Some(u) => self.push(Tok::Dur(n, u), line, col),
            None => {
                let hint = match suffix.as_str() {
                    "ct" => "cents are written 50c",
                    _ => "units are b, s, ms, us, ns, f, hz (db for gain; st, c, x for pitch)",
                };
                self.error(
                    Code::BadNumber,
                    line,
                    col,
                    format!("bad number: unknown unit '{suffix}'"),
                    Some(hint),
                )
            }
        }
    }

    fn dot(&mut self, line: u32, col: u32) {
        if self.peek(1) == Some('.') {
            self.bump();
            self.bump();
            self.push(Tok::DotDot, line, col);
        } else if self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            let d = self.digits();
            self.error(
                Code::BadNumber,
                line,
                col,
                format!("bad number '.{d}'"),
                Some(&format!("write 0.{d}")),
            );
        } else {
            self.bump();
            self.push(Tok::Dot, line, col);
        }
    }

    fn var(&mut self, line: u32, col: u32) {
        self.bump();
        let mut s = String::new();
        while let Some(c) = self
            .peek(0)
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        {
            self.bump();
            s.push(c);
        }
        if s.is_empty() || !s.starts_with(|c: char| c.is_ascii_lowercase()) {
            self.error(
                Code::UnknownChar,
                line,
                col,
                "'$' must be followed by a name".into(),
                None,
            );
        } else {
            self.push(Tok::Var(s), line, col);
        }
    }

    fn word(&mut self, line: u32, col: u32) {
        let mut w = String::new();
        let is_word_char = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
        while let Some(c) = self.peek(0).filter(is_word_char) {
            self.bump();
            w.push(c);
        }
        // a note with a sharp (`c#4`, `f#-1`) keeps its `#`: it is one word
        // when a note letter is followed at once by `#` and a digit (or `-`
        // and a digit). Anything else after `#` is a comment, as ever.
        if w.len() == 1
            && matches!(w.as_bytes()[0], b'a'..=b'g')
            && self.peek(0) == Some('#')
            && (self.peek(1).is_some_and(|c| c.is_ascii_digit())
                || (self.peek(1) == Some('-') && self.peek(2).is_some_and(|c| c.is_ascii_digit())))
        {
            self.bump();
            w.push('#');
            if self.peek(0) == Some('-') {
                self.bump();
                w.push('-');
            }
            while let Some(c) = self.peek(0).filter(is_word_char) {
                self.bump();
                w.push(c);
            }
        }
        if !w.starts_with(|c: char| c.is_ascii_lowercase())
            || w.contains(|c: char| c.is_ascii_uppercase())
        {
            self.error(
                Code::UnknownChar,
                line,
                col,
                format!("'{w}' is not a name: names are lowercase letters, digits and '_'"),
                None,
            );
            return;
        }
        match keyword(&w) {
            Some(k) => self.push(k, line, col),
            None => self.push(Tok::Name(w), line, col),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        let l = lex(src);
        assert!(l.errors.is_empty(), "{src}: {:?}", l.errors);
        l.tokens.into_iter().map(|t| t.kind).collect()
    }

    fn name(s: &str) -> Tok {
        Tok::Name(s.into())
    }

    #[test]
    fn lexer_c_sharp_is_a_word() {
        assert_eq!(
            kinds("freq=c#4"),
            [name("freq"), Tok::Eq, name("c#4"), Tok::Eof]
        );
        assert_eq!(
            kinds("f#-1 a3 eb2"),
            [name("f#-1"), name("a3"), name("eb2"), Tok::Eof]
        );
        // only a note letter takes the sharp, and only before a digit
        assert_eq!(kinds("x#4"), [name("x"), Tok::Eof]);
        assert_eq!(kinds("c#x"), [name("c"), Tok::Eof]);
        assert_eq!(kinds("c#"), [name("c"), Tok::Eof]);
        assert_eq!(kinds("h#4"), [name("h"), Tok::Eof]);
    }

    #[test]
    fn lexer_hash_after_space_is_comment() {
        assert_eq!(kinds("x #note"), [name("x"), Tok::Eof]);
        assert_eq!(kinds("lead # c"), [name("lead"), Tok::Eof]);
        assert_eq!(kinds("c #4"), [name("c"), Tok::Eof]);
        assert_eq!(kinds("kick #4 c#4"), [name("kick"), Tok::Eof]);
        assert_eq!(kinds("c # 4"), [name("c"), Tok::Eof]);
    }

    #[test]
    fn lexes_every_token_shape() {
        use Tok::*;
        assert_eq!(kinds("kick"), [name("kick"), Eof]);
        assert_eq!(kinds("kick.beat"), [name("kick"), Dot, name("beat"), Eof]);
        assert_eq!(kinds(r#""a\"b\\c""#), [Str("a\"b\\c".into()), Eof]);
        assert_eq!(kinds("0.5 12"), [Num(0.5), Num(12.0), Eof]);
        assert_eq!(kinds("$bpm"), [Var("bpm".into()), Eof]);
        assert_eq!(
            kinds("{ } | , ; = .. . % ~ ( ) + - * /"),
            [
                LBrace, RBrace, Pipe, Comma, Semi, Eq, DotDot, Dot, Percent, Tilde, LParen, RParen,
                Plus, Minus, Star, Slash, Eof
            ]
        );
        assert_eq!(
            kinds("a # comment\nb"),
            [name("a"), Newline, name("b"), Eof]
        );
        // `\` joins lines: no Newline token
        assert_eq!(kinds("a \\\n b"), [name("a"), name("b"), Eof]);
        assert_eq!(kinds("4~0.03"), [Num(4.0), Tilde, Num(0.03), Eof]);
        assert_eq!(
            kinds("1..16%70"),
            [Num(1.0), DotDot, Num(16.0), Percent, Num(70.0), Eof]
        );
        assert_eq!(
            kinds("voice from on in"),
            [
                Kind(super::Kind::Voice),
                Kw(super::Kw::From),
                Kw(super::Kw::On),
                Kw(super::Kw::In),
                Eof
            ]
        );
        // spans: line, column (1-based), length
        let l = lex("ab\n  cde");
        assert_eq!(
            l.tokens[0].span,
            Span {
                line: 1,
                col: 1,
                len: 2
            }
        );
        assert_eq!(
            l.tokens[2].span,
            Span {
                line: 2,
                col: 3,
                len: 3
            }
        );
    }

    #[test]
    fn clock_and_duration_tokens() {
        use Tok::*;
        assert_eq!(
            kinds("93b 500ms 0.5s 24000f ($bpm)b"),
            [
                Dur(93.0, DurUnit::Ticks),
                Dur(500.0, DurUnit::Ms),
                Dur(0.5, DurUnit::S),
                Dur(24000.0, DurUnit::Frames),
                LParen,
                Var("bpm".into()),
                RParen,
                name("b"),
                Eof
            ]
        );
        assert_eq!(
            kinds("2b 1.5s 5ms 200ns 480f 3us"),
            [
                Dur(2.0, DurUnit::Ticks),
                Dur(1.5, DurUnit::S),
                Dur(5.0, DurUnit::Ms),
                Dur(200.0, DurUnit::Ns),
                Dur(480.0, DurUnit::Frames),
                Dur(3.0, DurUnit::Us),
                Eof
            ]
        );
        // the old clock spellings are ordinary names now
        assert_eq!(
            kinds("b93 m500 f24000"),
            [name("b93"), name("m500"), name("f24000"), Eof]
        );
        assert_eq!(kinds("s24000"), [name("s24000"), Eof]);
        assert_eq!(kinds("b"), [name("b"), Eof]);
        assert!(!lex("2t").errors.is_empty());
        assert_eq!(kinds("t"), [name("t"), Eof]);
        assert_eq!(
            kinds("7st 50c 2b"),
            [
                Tok::Pitch(7.0, PitchUnit::Semitones),
                Tok::Pitch(50.0, PitchUnit::Cents),
                Dur(2.0, DurUnit::Ticks),
                Eof
            ]
        );
    }

    #[test]
    fn pitch_suffixes() {
        use super::PitchUnit::*;
        assert_eq!(
            kinds("7st 50c 1.5x"),
            [
                Tok::Pitch(7.0, Semitones),
                Tok::Pitch(50.0, Cents),
                Tok::Pitch(1.5, Ratio),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn token_errors() {
        let e = |s: &str| {
            let l = lex(s);
            let e = &l.errors[0];
            (e.code, e.span.line, e.span.col)
        };
        assert_eq!(e("\"abc"), (Code::UnclosedString, 1, 1));
        assert_eq!(e("x .5"), (Code::BadNumber, 1, 3));
        assert_eq!(e("3q"), (Code::BadNumber, 1, 1));
        assert_eq!(e("a @"), (Code::UnknownChar, 1, 3));
        assert_eq!(e("Kick"), (Code::UnknownChar, 1, 1));
    }

    #[test]
    fn continuation_at_end() {
        assert!(lex("x \\").continued_at_end);
        assert!(!lex("x \\\ny").continued_at_end);
    }
}
