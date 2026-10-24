# SHa: shell syntax (lexer, parser, expressions)
Status: draft   ·   Changelog at the bottom

Part 1 of 2 of SH (shell-core, wave 2). SHb follows in the same wave, on
the same files. The language is defined in
`/home/alexander-toth/rust_proj/blast/plans/detailed/SH-grammar.md`, which
needs user approval first (REQ-004). Implement against the approved version
of that file; if it was approved with changes, the planner refreshes this plan
before dispatch.

## Summary
A standalone `shell/` module that turns shell text into a syntax tree, with
precise error locations and an "incomplete input" signal for open blocks. It
also has a pure expression evaluator. Nothing runs scripts yet (SHb), and
nothing outside `shell/` uses it.

## Ownership
- Creates: `blast/src/shell/mod.rs` (replacing the scaffold stub),
  `shell/error.rs`, `shell/lexer.rs`, `shell/ast.rs`, `shell/parser.rs`,
  `shell/expr.rs`, `shell/value.rs`.
- Modifies: nothing outside `shell/**`.
- Reads only: nothing (the shell is standalone). `README.md` module list:
  add `shell/` (one line; SH owns that line).

## Registry entries
Added by the architect in the wave-2 scaffold:
- `blast/src/lib.rs`: `pub mod shell;`
- `blast/src/shell/mod.rs`: stub containing only `//! BLAST shell language (SH). See plans/detailed/SH-grammar.md.`

## Architecture changes
None in SHa. (SHb's additions to the §3.7 API are REQ-005.)

## Interfaces
- Provides (crate-internal to `shell`, `pub(crate)`):
  ```rust
  // error.rs
  pub struct ShellErr { pub src: String, pub line: u32, pub col: u32, pub kind: ShellErrKind }
  pub enum ShellErrKind { /* see step 1 */ }
  impl fmt::Display for ShellErr   // "src:line:col: message"
  impl std::error::Error for ShellErr

  // lexer.rs
  pub(crate) struct Span { pub start: usize, pub end: usize }        // byte offsets into the input
  pub(crate) enum Tok { Word(Word), Semi, Amp, LBrace, RBrace, Pipe, Newline }
  pub(crate) fn lex(src: &str) -> Result<Vec<(Tok, Span)>, LexErr>; // LexErr::Incomplete | LexErr::At(Span, ShellErrKind)

  // ast.rs
  pub(crate) struct Word { pub parts: Vec<WordPart>, pub span: Span, pub quoted: bool }
  pub(crate) enum WordPart { Lit(String), Var(String), Arith(Expr) }
  pub(crate) enum Stmt { Let{..}, If{..}, For{..}, Fn{..}, Source{..}, Wait{..}, Print{..},
                         Named{ name, body: Vec<Stmt> }, Linked(Vec<Simple>) }
  pub(crate) struct Simple { pub words: Vec<Word>, pub span: Span }
  pub(crate) enum ForItems { Range{ from: Expr, to: Expr, inclusive: bool, by: Option<Expr> }, Words(Vec<Word>) }

  // parser.rs
  pub(crate) enum ParseErr { Incomplete, At(Span, ShellErrKind) }
  pub(crate) fn parse(src: &str) -> Result<Vec<Stmt>, ParseErr>;
  pub(crate) fn line_col(src: &str, at: usize) -> (u32, u32);         // 1-based, col in chars

  // expr.rs + value.rs
  pub(crate) enum Expr { Num(f64), Str(String), Var(String), Unary(..), Binary(..), Range{..} }
  pub(crate) enum Value { Num(f64), Str(String), List(Vec<Value>) }
  pub(crate) fn parse_expr(src: &str, base: usize) -> Result<Expr, ParseErr>;   // base = offset for spans
  pub(crate) fn eval(e: &Expr, vars: &dyn Fn(&str) -> Option<Value>) -> Result<Value, ShellErrKind>;
  impl Value { pub fn truthy(&self) -> bool; pub fn to_word(&self) -> String; }
  ```
- Consumes: nothing.
- Contract issues: none. `ShellErr` gets the shape README §3.7 leaves open
  (fields above); that's part of REQ-005.

## Steps
1. **`error.rs`.** `ShellErr` and `ShellErrKind` with a hand-written
   `Display` (README §1.3). Kinds: `UnexpectedToken(String)`,
   `UnexpectedEof(&'static str)` (what was open), `UnterminatedQuote`,
   `Reserved(String)` (word, e.g. `all`), `PipeReserved`, `BadName(String)`,
   `UndefinedVar(String)`, `TypeMismatch(&'static str)`, `DivByZero`,
   `RangeTooLong`, `BadWait(String)`, `WaitInBatch`, `NestedNamedBlock`,
   `WhitespaceInWord(String)`, `Arity{ name, want, got }`, `RecursionLimit`,
   `SourceDepth`, `SourceIo{ path, err: String }`, `OutputLimit`. (SHb uses
   the run-time kinds; define them all now so the enum is settled.)
   `mod.rs` declares the submodules and re-exports `ShellErr`/`ShellErrKind`.
2. **`lexer.rs`.** Hand-written over `char_indices()`. Rules from the
   grammar §1: newline/`;`/`&`/`{`/`}` operators; `|` only when standalone
   (preceded by whitespace or start, followed by whitespace or end);
   `#` comment at word start; `\` + newline is skipped; `"…"` with `\"`,
   `\\`, `\$` escapes and `$` expansion; `'…'` literal; inside a word,
   `$name`, `${name}`, and `$((` … matching `))` (nesting-aware, and spaces
   inside don't end the word). The `$((…))` text is handed to
   `expr::parse_expr` with its byte offset, giving `WordPart::Arith`.
   End of input inside `$((`, or after a trailing `\`, is `LexErr::Incomplete`.
   An unterminated quote is an error, not incomplete (quotes can't span lines).
3. **`value.rs`.** `Value` with `truthy` and `to_word`: integer-valued
   finite numbers format as `i64` when they fit (`16`), others with `{}`
   (`0.25`); lists join elements with `,`; strings as-is.
4. **`expr.rs`.** A tiny expression lexer (numbers, `"strings"`, names,
   `$name`, operators `+ - * / % == != < <= > >= .. ..= ( )`, keywords
   `and or not by`), then precedence climbing exactly as the EBNF. `eval`:
   numeric ops on numbers; `+` on two strings concatenates; comparisons need
   matching types (strings compare with `==`/`!=` only); `%` and `/` by zero
   is `DivByZero`; a range yields `Value::List` of numbers, `by` defaults
   to 1, a step of 0 or a wrong-signed step is an error, and more than 65 536
   elements is `RangeTooLong` (check the count before allocating).
5. **`ast.rs` + `parser.rs`.** Recursive descent over the token list.
   Statement keywords are recognised only when they're the first word of a
   statement and the word is unquoted and a plain literal. Specifics:
   - `let NAME = …`: the expression is the source text from after `=` to
     the statement end, parsed with `parse_expr` (spans keep the right
     columns). Reject reserved names (`BadName`/`Reserved`).
   - `if`: the condition is the source text up to the `{` token. `else`
     may follow `}` after newlines.
   - `for NAME in …`: if the header words form `A..B`, `A..=B`, optionally
     `by C` (detected on the raw text up to `{`), it's `ForItems::Range`,
     otherwise `ForItems::Words`.
   - `fn NAME params… {`; `source WORD`; `wait WORD`; `print WORDS`.
   - `NAME {` at statement start is a named block; nested named blocks
     are rejected at parse time (`NestedNamedBlock`), and so is `wait`
     directly inside one (`WaitInBatch`) (function calls are checked by SHb
     at run time).
   - `simple { & NL* simple }` builds `Stmt::Linked`; a trailing `&` at
     the end of input is `Incomplete`. A `Pipe` token is `PipeReserved`.
   - End of input with a `{` open is `Incomplete`. A stray `}` is
     `UnexpectedToken`.
   - `line_col` converts a byte offset to 1-based line and char column.
6. Unit tests (below) in each file's `#[cfg(test)] mod tests`.

## Tests
`lexer.rs`:
- `lex_words_keep_command_punctuation`: `seq k -s 0,3 -c 1-4:50 -t c:main drums.kick` gives one word per field.
- `lex_pipe_inside_word_is_literal`: `-j 0:0.1|0.2` is one word; `a | b` gives `Pipe`.
- `lex_operators`: `a;b&c{d}` gives words and `Semi Amp LBrace RBrace`.
- `lex_comment_only_at_word_start`: `a#b # c` is one word `a#b`.
- `lex_quotes_and_escapes`: `"x $v \$y"` has parts `Lit("x ") Var(v) Lit(" $y")`; `'$v'` is literal.
- `lex_var_forms`: `$v.seq1`, `${v}x`, `b:$((1 + 2))` split into the right parts.
- `lex_incomplete`: trailing `\`, open `$((` are `Incomplete`; `"abc` is `UnterminatedQuote`.
- `lex_unicode_columns`: an error after `é` reports the char column, not the byte column.

`expr.rs`:
- `expr_precedence`: `1 + 2 * 3 == 7 and not 0` is truthy.
- `expr_ranges`: `0..4`, `0..=4`, `0..16 by 4`, `4..0 by -1`; `0..4 by 0` errors; `0..1e9` is `RangeTooLong`.
- `expr_strings`: concatenation, `==`, and `"a" < 1` is `TypeMismatch`.
- `expr_div_by_zero`: `/` and `%`.
- `expr_bare_and_dollar_names`: `x + $x` with x = 2 is 4; an undefined name is `UndefinedVar`.

`value.rs`:
- `value_to_word_formats_integers_plainly`: 16.0 → `16`, 0.25 → `0.25`, -3.0 → `-3`, list → `0,4,8`.

`parser.rs`:
- `parse_each_example_script`: the 10 example scripts in `SH-grammar.md` §9 parse (they're copied into the test as string constants).
- `parse_rejects_examples`: each "rejected example" that is a parse-time error gives its kind and the right line/column.
- `parse_incomplete_inputs`: `kick {`, `for v in a b {\n load $v`, `a &`, `if 1 {\n} else` are `Incomplete`.
- `parse_named_block_and_linked`: structure of `kick { a & b\n c }`.
- `parse_for_range_vs_words`: `for i in 0..8 by 2 {}` is a range; `for v in a b {}` is words.
- `parse_keywords_only_at_statement_start`: `load let` is a command line; `"let" x` is a command line.
- `parse_reserved_names`: `let all = 1`, `fn wait {}` are errors.

## Done when
- README §1.4, plus: everything above `pub(crate)` is used by tests, and no
  `#[allow(dead_code)]` except on items SHb will use (each marked
  `// used by SHb`).
- `cargo clippy` is clean in `shell/**`.

## Risks and open questions
- The grammar itself is pending (REQ-004). Decisions D-1..D-7 change small,
  local parts of the lexer/parser; none changes the module structure.
- Re-lexing the whole accumulated buffer on every `feed` (SHb) is quadratic
  in block length. Blocks typed at a prompt are short, and files are parsed
  once, so this is fine.

## Size
M: about 900 lines including tests; hand-written but conventional
recursive descent.

## Changelog
- 2026-10-24: first draft.
