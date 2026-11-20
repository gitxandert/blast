//! A small JSON reader and writer for the `--agent` protocol: flat objects
//! of strings, numbers, booleans and null, with arrays and nesting read
//! (to 32 deep) so unknown keys can hold anything.

use std::fmt;

/// The deepest nesting the reader accepts.
const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// The value of `key`, if this is an object that has it (the last one
    /// wins when a key repeats).
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Why a line isn't JSON: what, and the 1-based column (in characters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonErr {
    pub col: usize,
    pub message: String,
}

impl fmt::Display for JsonErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (column {})", self.message, self.col)
    }
}

impl std::error::Error for JsonErr {}

/// Reads `line` as one JSON value; anything after it but whitespace is an
/// error.
pub fn parse(line: &str) -> Result<Json, JsonErr> {
    let mut p = Reader {
        chars: line.chars().collect(),
        at: 0,
    };
    p.ws();
    let value = p.value(0)?;
    p.ws();
    if p.at < p.chars.len() {
        return Err(p.err("unexpected text after the value"));
    }
    Ok(value)
}

struct Reader {
    chars: Vec<char>,
    at: usize,
}

impl Reader {
    fn err(&self, message: &str) -> JsonErr {
        JsonErr {
            col: self.at + 1,
            message: message.to_string(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self, w: &str, v: Json) -> Result<Json, JsonErr> {
        for c in w.chars() {
            if !self.eat(c) {
                return Err(self.err(&format!("expected '{w}'")));
            }
        }
        Ok(v)
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonErr> {
        match self.peek() {
            None => Err(self.err("expected a value")),
            Some('{') | Some('[') if depth >= MAX_DEPTH => {
                Err(self.err("nested more than 32 deep"))
            }
            Some('{') => self.object(depth),
            Some('[') => self.array(depth),
            Some('"') => self.string().map(Json::Str),
            Some('t') => self.word("true", Json::Bool(true)),
            Some('f') => self.word("false", Json::Bool(false)),
            Some('n') => self.word("null", Json::Null),
            Some('-' | '0'..='9') => self.number(),
            Some(_) => Err(self.err("expected a value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonErr> {
        self.at += 1;
        let mut members = Vec::new();
        self.ws();
        if self.eat('}') {
            return Ok(Json::Obj(members));
        }
        loop {
            self.ws();
            if self.peek() != Some('"') {
                return Err(self.err("expected a key"));
            }
            let key = self.string()?;
            self.ws();
            if !self.eat(':') {
                return Err(self.err("expected ':'"));
            }
            self.ws();
            members.push((key, self.value(depth + 1)?));
            self.ws();
            if self.eat(',') {
                continue;
            }
            if self.eat('}') {
                return Ok(Json::Obj(members));
            }
            return Err(self.err("expected ',' or '}'"));
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonErr> {
        self.at += 1;
        let mut items = Vec::new();
        self.ws();
        if self.eat(']') {
            return Ok(Json::Arr(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            if self.eat(',') {
                continue;
            }
            if self.eat(']') {
                return Ok(Json::Arr(items));
            }
            return Err(self.err("expected ',' or ']'"));
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonErr> {
        let mut n = 0;
        for _ in 0..4 {
            let Some(d) = self.peek().and_then(|c| c.to_digit(16)) else {
                return Err(self.err("expected 4 hex digits after \\u"));
            };
            n = n * 16 + d;
            self.at += 1;
        }
        Ok(n)
    }

    fn string(&mut self) -> Result<String, JsonErr> {
        self.at += 1; // the opening quote
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("the string never ends"));
            };
            match c {
                '"' => {
                    self.at += 1;
                    return Ok(out);
                }
                '\\' => {
                    self.at += 1;
                    let Some(e) = self.peek() else {
                        return Err(self.err("the string never ends"));
                    };
                    self.at += 1;
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) {
                                // a high surrogate needs its low half
                                if !(self.eat('\\') && self.eat('u')) {
                                    return Err(self.err("a lone surrogate"));
                                }
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("a lone surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return Err(self.err("a lone surrogate"));
                            } else {
                                hi
                            };
                            match char::from_u32(code) {
                                Some(ch) => out.push(ch),
                                None => return Err(self.err("not a character")),
                            }
                        }
                        _ => {
                            self.at -= 1;
                            return Err(self.err("unknown escape"));
                        }
                    }
                }
                c if (c as u32) < 0x20 => {
                    return Err(self.err("a control character must be escaped"));
                }
                c => {
                    out.push(c);
                    self.at += 1;
                }
            }
        }
    }

    fn digits(&mut self) -> usize {
        let from = self.at;
        while matches!(self.peek(), Some('0'..='9')) {
            self.at += 1;
        }
        self.at - from
    }

    // RFC 8259: -? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?
    fn number(&mut self) -> Result<Json, JsonErr> {
        let from = self.at;
        self.eat('-');
        match self.peek() {
            Some('0') => {
                self.at += 1;
                if matches!(self.peek(), Some('0'..='9')) {
                    return Err(self.err("a number can't start with 0 here"));
                }
            }
            Some('1'..='9') => {
                self.digits();
            }
            _ => return Err(self.err("expected a digit")),
        }
        if self.eat('.') && self.digits() == 0 {
            return Err(self.err("expected a digit after '.'"));
        }
        if self.eat('e') || self.eat('E') {
            if !self.eat('+') {
                self.eat('-');
            }
            if self.digits() == 0 {
                return Err(self.err("expected a digit in the exponent"));
            }
        }
        let text: String = self.chars[from..self.at].iter().collect();
        match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Json::Num(n)),
            _ => Err(self.err("number out of range")),
        }
    }
}

/// Appends `s` as a quoted JSON string: `"` and `\` escaped, control
/// characters as `\n`, `\t`, … or `\u00XX`.
pub fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Appends `v` as written (numbers that are whole print without a point).
pub fn write_json(out: &mut String, v: &Json) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&format!("{n}"));
            }
        }
        Json::Str(s) => write_str(out, s),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, item);
            }
            out.push(']');
        }
        Json::Obj(members) => {
            out.push('{');
            for (i, (k, item)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write_json(out, item);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_request() {
        let j = parse(r#" {"id": 7, "script": "state", "x": null, "y": [1, true]} "#).unwrap();
        assert_eq!(j.get("id"), Some(&Json::Num(7.0)));
        assert_eq!(j.get("script"), Some(&Json::Str("state".into())));
        assert_eq!(j.get("x"), Some(&Json::Null));
        assert_eq!(
            j.get("y"),
            Some(&Json::Arr(vec![Json::Num(1.0), Json::Bool(true)]))
        );
        assert_eq!(j.get("nope"), None);
        assert_eq!(parse("{}").unwrap(), Json::Obj(vec![]));
    }

    #[test]
    fn parses_escapes_and_unicode() {
        assert_eq!(
            parse(r#""a\n\t\"\\\/\b\f\r""#).unwrap(),
            Json::Str("a\n\t\"\\/\u{8}\u{c}\r".into())
        );
        assert_eq!(parse(r#""é😀""#).unwrap(), Json::Str("é😀".into()));
        assert_eq!(parse(r#""é""#).unwrap(), Json::Str("é".into()));
        // a surrogate pair
        assert_eq!(parse(r#""😀""#).unwrap(), Json::Str("😀".into()));
        assert!(parse(r#""\ud83d""#).is_err());
        assert!(parse(r#""\ude00""#).is_err());
        assert!(parse(r#""\x""#).is_err());
        assert!(parse("\"a\nb\"").is_err());
    }

    #[test]
    fn rejects_trailing_garbage_and_deep_nesting() {
        let e = parse("{} x").unwrap_err();
        assert_eq!(e.col, 4);
        assert!(parse("{\"a\":1,}").is_err());
        assert!(parse("[1 2]").is_err());
        assert!(parse("").is_err());
        let ok = format!("{}{}", "[".repeat(32), "]".repeat(32));
        assert!(parse(&ok).is_ok());
        let deep = format!("{}{}", "[".repeat(33), "]".repeat(33));
        assert!(parse(&deep).is_err());
        let deep = format!("{}1{}", "{\"a\":".repeat(40), "}".repeat(40));
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn numbers_per_rfc() {
        assert_eq!(parse("-0.5e3").unwrap(), Json::Num(-500.0));
        assert_eq!(parse("0").unwrap(), Json::Num(0.0));
        assert_eq!(parse("12E+2").unwrap(), Json::Num(1200.0));
        for bad in ["01", ".5", "-", "1.", "1e", "+1", "--1", "1e999"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn writer_escapes() {
        let mut s = String::new();
        write_str(&mut s, "a\"b\\c\nd\u{1}é");
        assert_eq!(s, "\"a\\\"b\\\\c\\nd\\u0001é\"");
        let mut s = String::new();
        write_json(&mut s, &Json::Num(7.0));
        s.push(' ');
        write_json(&mut s, &Json::Num(0.5));
        assert_eq!(s, "7 0.5");
        // what the writer writes, the reader reads
        let mut s = String::new();
        write_str(&mut s, "x\u{1f}\"\\\t");
        assert_eq!(parse(&s).unwrap(), Json::Str("x\u{1f}\"\\\t".into()));
    }
}
