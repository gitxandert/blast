# SH grammar proposal: the BLAST shell language
Status: draft (needs user approval: REQ-004)   ·   Changelog at the bottom

This is the first deliverable of SH (brief PF, README §4 item 2). It is the
concrete syntax the shell (`shell/**`) accepts. SH is implemented against
this file once the user approves it. Every decision point is marked
**[D-n]**. Each one has a recommended answer, and the user can change any of
them independently (REQ for the grammar lists them).

## 0. What the shell is

The shell turns text into a flat stream of outputs (README §3.7):

```rust
pub enum ShellOut { Line(String), Batch(Vec<String>), Wait(Wait), Print(String) }
```

- `Line`: one plain command line for `CmdProcessor` (e.g. `load kick -t b:120`).
- `Batch`: several command lines that must apply atomically (`Command::Batch`).
- `Wait`: a pause. The REPL's script runner sleeps it; the shell never sleeps.
- `Print`: text for the terminal.

The shell knows nothing about engine semantics. It never checks whether
`load` or `kick` exist. A command line is just words, joined by single
spaces after expansion. Because nothing the shell does depends on what the
engine says, a whole script can be expanded up front and then played back
with its waits (REPL's runner does exactly that).

## 1. Lexical structure

- **Statements** end at a newline or `;`.
- **Comments**: `#` at the start of a word runs to the end of the line
  (`a#b` is a plain word).
- **Line continuation**: a `\` immediately before the newline joins the next
  line. A line also continues automatically while a `{` is open, after a
  trailing `&`, and inside an unclosed `$(( … ))`.
- **Operators** (always, wherever they appear unquoted): `;` `&` `{` `}`.
- **`|`** is an operator only when it stands alone between whitespace. Inside
  a word it is an ordinary character, because the command layer already uses
  it: `seq kick -s 0,4 -j 0:0.1|0.2` passes through unchanged. **[D-1]**
- **Words** are everything else: runs of non-whitespace, non-operator
  characters. `-t`, `b:120`, `0,3,4`, `1-4:50`, `drums.kick`, `0.1|0.2` are
  each one word. Commas, colons, dots and dashes have no meaning to the shell.
- **Quotes**: `"…"` groups text into one word and expands `$` inside it;
  `'…'` is literal. `\"`, `\\`, `\$` escape inside double quotes. A quote
  can't span lines.
- **Expansion inside words**: `$name`, `${name}`, and `$(( expr ))`. A `$name`
  ends at the first character that can't be in a name, so `$v.seq1` is
  variable `v` followed by `.seq1`. `\$` is a literal dollar.
- **Names** (variables, functions, block names): `[A-Za-z_][A-Za-z0-9_]*`.
- **Reserved words**: `let if else for in by fn source wait print all`. They
  can't be used as variable or function names. Only `let if for fn source
  wait print` start a statement; `else in by` are only meaningful in their
  constructs; `all` is reserved for the command layer (§6).

## 2. EBNF

Terminals in quotes; `WORD` is a lexer word (§1), `NAME` a name, `NL` a
newline. `sep` is one or more of `;`/`NL`.

```ebnf
script      = { sep } [ stmt { sep stmt } ] { sep } ;

stmt        = let_stmt
            | if_stmt
            | for_stmt
            | fn_def
            | source_stmt
            | wait_stmt
            | print_stmt
            | named_block
            | linked ;

let_stmt    = "let" NAME "=" expr ;
if_stmt     = "if" expr block { "else" "if" expr block } [ "else" block ] ;
for_stmt    = "for" NAME "in" for_items block ;
for_items   = range_form | WORD { WORD } ;
range_form  = atom ( ".." | "..=" ) atom [ "by" atom ] ;   (* only when it is the whole header *)
fn_def      = "fn" NAME { NAME } block ;
source_stmt = "source" WORD ;
wait_stmt   = "wait" WORD ;                                (* see §5 *)
print_stmt  = "print" { WORD } ;
named_block = NAME block ;                                 (* [D-2] *)
block       = "{" script "}" ;

linked      = simple { "&" { NL } simple } ;               (* atomic batch, [D-3] *)
simple      = WORD { WORD } [ pipe_reserved ] ;            (* command line or fn call *)
pipe_reserved = "|" simple ;                               (* reserved: error today, [D-4] *)

(* expressions: in let, if, for ranges, and $(( … )) *)
expr        = or_expr ;
or_expr     = and_expr { "or" and_expr } ;
and_expr    = not_expr { "and" not_expr } ;
not_expr    = [ "not" ] cmp_expr ;
cmp_expr    = range_expr [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) range_expr ] ;
range_expr  = add_expr [ ( ".." | "..=" ) add_expr [ "by" add_expr ] ] ;
add_expr    = mul_expr { ( "+" | "-" ) mul_expr } ;
mul_expr    = unary { ( "*" | "/" | "%" ) unary } ;
unary       = [ "-" ] atom ;
atom        = NUMBER | STRING | NAME | "$" NAME | "(" expr ")" ;
```

Notes on the grammar:
- In an expression a bare `NAME` and `$NAME` both read the variable. Literal
  text must be quoted: `let v = "kick"`.
- `if` conditions and `let` right-hand sides run to the `{` or the end of the
  statement. The lexer hands that span to the expression parser, so
  `if $n < 3 {` works (`<` is a plain word to the word lexer; the shell has
  no redirection).
- A statement that starts with a function's name is a call; anything else
  that is a `simple` is a command line. Functions can shadow commands.

## 3. Values and expansion

- Three value types: **number** (f64), **string**, **list** (of numbers or
  strings). Ranges make lists: `0..4` is `0,1,2,3`, `0..=4` includes 4,
  `0..16 by 4` is `0,4,8,12`. A range has at most 65 536 elements.
- Arithmetic is on numbers only; `+` on two strings concatenates. Comparing
  a number with a string is an error. Division by zero is an error.
- **Truthiness** (`if`): a non-zero number, a non-empty string, a non-empty list.
- **Expanding into a word**: an integer-valued number prints with no decimal
  point (`16`, not `16.0`, so `-p $((4*4))` parses as an integer); other
  numbers use Rust's shortest `f64` form (`0.25`). A list joins with `,`,
  which is the command layer's list convention: `-s $steps` becomes
  `-s 0,4,8,12`. **[D-5]**
- An undefined variable is an error (not an empty string, unlike Bash).
- A quoted word that still contains whitespace after expansion is an error
  in a command line, since the command layer splits on whitespace. It's
  fine in `print`.

## 4. Statements

- **`let x = expr`** assigns. At top level it sets a global. Inside a
  function it sets a local of that call. `bpm` is predefined (120) and is
  used by `wait … b` (§5).
- **`if … { } else if … { } else { }`**: as usual. `else` may follow `}` on
  the same line or the next one.
- **`for x in kick snare hat { … }`** iterates words (each expanded; a list
  value splices its elements). **`for i in 0..8 by 2 { … }`** iterates a
  range. The loop variable stays defined afterwards, like Bash.
- **`fn name a b { … }`** defines a function with parameters `a`, `b`.
  `name x y` calls it with `$a = x`, `$b = y`. A wrong argument count is an
  error. The body is expanded at call time. Calls nest at most 64 deep (no
  `while` exists, so this bounds every script).
- **`source file`** reads and runs another script in the same shell, so its
  variables and functions stay defined (Bash semantics). Relative paths
  resolve against the sourcing file's directory, or the working directory
  at the prompt. Nesting is limited to 16 deep (catches cycles).
- **`print words…`** emits `Print` with the expanded words joined by spaces.
- **`a & b & c`** links commands into one `Batch`, applied atomically within
  one audio period. A single command is a `Line`, never a one-element
  `Batch`. A function call inside a link contributes its command lines.
  A `wait` inside a link is an error. **[D-3]**
- **`name { … }`**: a *named block*. Every command line inside becomes one
  atomic `Batch`, and `$self` is set to `name` inside it. `wait` and nested
  named blocks are errors inside it. This is where `|` pipelines will live
  (`kick { seq … | trig }`, from `notes.txt`). **[D-2]**
- Total output of one `feed` is capped at 100 000 items (an error beyond it).

## 5. `wait`

`wait <amount><unit>`, after expansion, where the unit is:

| Form | Meaning |
|---|---|
| `wait 250` or `wait 250ms` | milliseconds |
| `wait 1.5s` | seconds |
| `wait 4b` | beats at the shell's `bpm` variable (default 120) |

```rust
pub enum Wait { Millis(f64), Beats { beats: f64, bpm: f64 } }
```

**[D-6]** Beats are of the *shell's own* `bpm`, not of an engine tempo
context. The shell can't see engine tempos, and the value is fixed when the
line is expanded. Reserved for later: `wait 4b@main` would wait 4 beats of
tempo context `main`, looked up by the REPL's runner in the mirror at run
time. Today `@` in a wait argument is an error saying it's reserved.

Waits are relative to the previous wait's deadline, not to "now", so a loop
of `wait 1b` doesn't drift (REPL's runner keeps the timeline).

## 6. Reserved for the command layer: `all`

**[D-7]** `all` passes through the shell untouched (`stop -v all` is the
line `stop -v all`). The command layer expands it, because only it knows the
names *at the moment the line runs*. The shell expands a script before
earlier lines have run, so a shell-side `all` would miss voices that the
same script loads. The shell only forbids `all` as a variable or function
name. (Who implements `all` in the command layer is a separate request.)

## 7. Reserved: pipes

**[D-4]** `a | b` (a standalone `|`) will mean: the events produced by
process `a` drive process `b`, e.g. `seq -s 0,4,8,12 | trig` (the seq is a
"bang machine" and `trig` resets a sample on each bang). That needs engine
work that is not in this roadmap. Today the shell rejects it with
`pipes are reserved and not implemented yet`. When they arrive, the shell
will hand a pipeline to the command layer as one unit (most likely a new
`ShellOut` variant), still without knowing what the stages mean.

## 8. Errors

Every error carries the source name, line and column, and prints as
`kick.blast:12:7: undefined variable 'bmp'`. At the prompt the source is
`<repl>` and lines count from the start of the current (possibly
multi-line) statement. A sourced file reports its own name and line.

## 9. Example scripts

Commands use today's syntax (`load`, `start -v`, `tc`, `group -v a,b`,
`seq -s`). The shell output is shown after `=>`.

**1. Plain lines and comments**
```sh
# load two tracks on a shared tempo
tc main b:120
load kick -t c:main
load snare -t c:main
```
=> `Line("tc main b:120")`, `Line("load kick -t c:main")`, `Line("load snare -t c:main")`

**2. Atomic start with `&`**
```sh
start -v kick & start -v snare
```
=> `Batch(["start -v kick", "start -v snare"])`

**3. Variables and arithmetic**
```sh
let bpm = 96
let half = $bpm / 2
tc slow b:$((half))
load pad -t b:${bpm}
```
=> `Line("tc slow b:48")`, `Line("load pad -t b:96")`

**4. Ranges as step lists**
```sh
let four = 0..16 by 4
seq kick -p 16 -s $four
seq hat -p 16 -s $((0..16 by 2)) -c a:70
```
=> `Line("seq kick -p 16 -s 0,4,8,12")`, `Line("seq hat -p 16 -s 0,2,4,6,8,10,12,14 -c a:70")`

**5. `for` over names**
```sh
for v in kick snare hat {
    load $v -t c:main
}
group drums -t c:main -v kick,snare,hat
```
=> three `load` lines, then the `group` line.

**6. Functions**
```sh
fn four_floor v {
    load $v -t c:main
    seq $v -p 16 -s 0,4,8,12
}
four_floor kick
four_floor clap
```
=> `load kick …`, `seq kick …`, `load clap …`, `seq clap …`

**7. Named block (atomic set-up)**
```sh
kick {
    load $self -t c:main
    seq $self -p 16 -s 0,3,8,11
    start -v $self
}
```
=> `Batch(["load kick -t c:main", "seq kick -p 16 -s 0,3,8,11", "start -v kick"])`

**8. A timed arrangement**
```sh
let bpm = 120
start -g drums
wait 16b
start -v bass & velocity -v bass 0.5
wait 8b
stop -g drums
print "breakdown"
```
=> `Line`, `Wait(Beats{16,120})`, `Batch([…])`, `Wait(Beats{8,120})`, `Line`, `Print("breakdown")`

**9. `if` and ranges with a step variable**
```sh
let dense = 1
for i in 0..4 {
    if dense and $i % 2 == 0 {
        velocity -v hat $((0.5 + $i / 8))
    } else {
        velocity -v hat 0.25
    }
    wait 1b
}
```
=> four `velocity` lines (`0.5`, `0.25`, `0.75`, `0.25`), each followed by `Wait(Beats{1,120})`

**10. Sourcing a library, long lines, and `all`**
```sh
source lib/drums.blast          # defines four_floor
four_floor kick
seq hat -p 16 \
    -s 0,2,4,6,8,10,12,14 \
    -j a:0.01|0.02
wait 2s
stop -v all
```
=> lines from `lib/drums.blast`, then `seq hat -p 16 -s 0,2,4,6,8,10,12,14 -j a:0.01|0.02`, `Wait(Millis(2000))`, `Line("stop -v all")`

**Rejected examples** (each is an error with a location):
`let all = 1` (reserved), `seq … | trig` (pipes reserved), `kick { wait 1b }`
(wait in an atomic block), `load "my kick"` (whitespace in a command word),
`print $nope` (undefined variable), `fn f { f }` then `f` (recursion limit).

## 10. Left out on purpose

`while`/`loop` (every script stays finite, so it can be expanded up front;
an endless live-coding loop would need lazy playback and is a follow-up),
command substitution `$(…)` (the shell can't see engine output), string
lists as literals, redirection, globbing, `return` values, aliases (a
one-line `fn` does the job).

## Changelog
- 2026-10-24: first draft.
