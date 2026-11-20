//! LANGd's tests: variables, scripts, `source`, `state`, `help` and the card.

use super::ast::Kind;
use super::card::{EXAMPLES, card};
use super::help::kinds;
use super::schema::{ACTIONS, kind_word, schema};
use super::{Code, LangErr, Parse, Source, parse};
use crate::audio_processing::commands::testing::*;
use crate::audio_processing::commands::*;
use crate::audio_processing::engine::VoiceParam;
use std::path::{Path, PathBuf};

/// A temp directory removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("blast-langd-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn lang_errs(e: CmdErr) -> Vec<LangErr> {
    match e {
        CmdErr::Lang(errs) => errs,
        other => panic!("not a language error: {other}"),
    }
}

fn codes(errs: &[LangErr]) -> Vec<Code> {
    errs.iter().map(|e| e.code).collect()
}

fn drain(rx: &mut CmdReceiver) -> Vec<Command> {
    std::iter::from_fn(|| rx.try_pop()).collect()
}

// ---- variables ----

#[test]
fn let_and_vars() {
    let mut cp = lang_processor();
    ok(
        &mut cp,
        "let bpm = 96\nclock slow ($bpm)b\nlet g = (0.5 * 3)\nvoice kick from \"kick\"",
    );
    let slow = cp.engine_state.context_id("slow").unwrap();
    assert_eq!(cp.engine_state.tempos[&slow].interval, 96.0);

    // `$g` is a number in a setting; the mirror keeps the value
    ok(&mut cp, "kick rate=$g");
    assert_eq!(
        cp.engine_state.voices[&vid(&cp, "kick")].params[VoiceParam::Rate.index()],
        1.5
    );
    local(&mut cp, "let g = ($g * 2 - 1)");
    ok(&mut cp, "kick rate=$g");
    assert_eq!(
        cp.engine_state.voices[&vid(&cp, "kick")].params[VoiceParam::Rate.index()],
        2.0
    );

    // steps and periods take them too
    ok(
        &mut cp,
        "let n = 3\nvoice hat from \"hat\"\nhat { seq b $n%50 in 4 }",
    );
    assert_eq!(
        cp.engine_state.procs.values().last().unwrap().seq.steps[0].at,
        2.0
    );

    // unknown, with a suggestion
    let errs = lerr(&mut cp, "kick rate=$nope");
    assert_eq!(codes(&errs), [Code::UnknownName]);
    let errs = lerr(&mut cp, "clock x ($bmp)b");
    assert_eq!(codes(&errs), [Code::UnknownName]);
    assert_eq!(errs[0].suggestion.as_deref(), Some("did you mean 'bpm'?"));

    // a failed line keeps neither its objects nor its variables
    let errs = lerr(&mut cp, "let z = 1\nclock y ($nope)b");
    assert_eq!(codes(&errs), [Code::UnknownName]);
    assert!(cp.vars.get("z").is_none());
    assert!(cp.engine_state.context_id("y").is_err());

    // division by zero, and a clock where a number is wanted
    assert_eq!(codes(&lerr(&mut cp, "let h = (1 / 0)")), [Code::OutOfRange]);
    local(&mut cp, "let c = 93b");
    assert_eq!(codes(&lerr(&mut cp, "kick rate=$c")), [Code::WrongType]);
    assert_eq!(codes(&lerr(&mut cp, "let s = \"x\"")), [Code::WrongType]);
}

// ---- scripts ----

#[test]
fn script_checked_whole() {
    let dir = TempDir::new("whole");
    let path = dir.write(
        "bad.blast",
        "voice kick from \"kick\"\n\
         voice hta from \"nope\"\n\
         kick { seq beat 1 2 3 9 in 4 }\n\
         voice hat from \"hat\"\n",
    );
    let mut cp = lang_processor();
    let (mut tx, mut rx) = cmd_queue(16);

    let errs = lang_errs(cp.run_script(&path, &mut tx).unwrap_err());
    // every error, with its line; the good lines around them are fine
    assert_eq!(codes(&errs), [Code::UnknownName, Code::StepOutsideCycle]);
    assert_eq!(errs[0].span.line, 2);
    assert_eq!(errs[1].span.line, 3);
    let shown = errs[0].to_string();
    assert!(shown.contains("bad.blast:2:"), "{shown}");
    assert!(shown.contains("E301"), "{shown}");

    // nothing applied or queued
    assert!(cp.engine_state.voices.is_empty());
    assert!(drain(&mut rx).is_empty());

    // `check` says the same and changes nothing either
    let errs = lang_errs(cp.check_script(&path).unwrap_err());
    assert_eq!(errs.len(), 2);
    assert!(cp.engine_state.voices.is_empty());
}

#[test]
fn script_applies_line_by_line() {
    let dir = TempDir::new("lines");
    let path = dir.write(
        "good.blast",
        "# a demo\n\
         let bpm = 100\n\
         voice kick from \"kick\"\n\
         voice hat from \"hat\" and voice snare from \"snare\"\n\
         group drums { kick; hat }\n\
         start drums\n",
    );
    let mut cp = lang_processor();
    let (mut tx, mut rx) = cmd_queue(16);
    cp.run_script(&path, &mut tx).unwrap();

    let cmds = drain(&mut rx);
    assert_eq!(cmds.len(), 4);
    assert!(matches!(cmds[0], Command::Load(_)));
    assert!(matches!(&cmds[1], Command::Batch(b) if b.cmds.len() == 2));
    assert!(matches!(cmds[2], Command::Group(_)));
    assert!(matches!(cmds[3], Command::Start(_)));
    assert_eq!(cp.engine_state.voices.len(), 3);
    assert!(cp.vars.get("bpm").is_some());
}

#[test]
fn script_check_reports_warnings_and_open_blocks() {
    let dir = TempDir::new("warn");
    let mut cp = lang_processor();
    ok(&mut cp, "voice kick from \"kick\"");
    let path = dir.write("w.blast", "voice kick from \"kick\"\n");
    let warns = cp.check_script(&path).unwrap();
    assert_eq!(codes(&warns), [Code::ChangesNothing]);
    assert_eq!(warns[0].file.as_deref(), Some(path.to_str().unwrap()));

    // a file that ends inside a block is E202, at the block
    let path = dir.write("open.blast", "voice a from \"kick\"\ngroup g {\n  a\n");
    let errs = lang_errs(cp.check_script(&path).unwrap_err());
    assert_eq!(codes(&errs), [Code::UnclosedBlock]);
    assert_eq!((errs[0].span.line, errs[0].span.col), (2, 9));

    // `check "file"` in the language
    let path = dir.write("ok.blast", "voice hat from \"hat\"\n");
    let text = local(&mut cp, &format!("check \"{}\"", path.display()));
    assert_eq!(text, "ok");
    assert!(!cp.engine_state.voice_names.contains_key("hat"));
}

#[test]
fn source_relative_cycle_depth() {
    let dir = TempDir::new("source");
    let a = dir.write(
        "a.blast",
        "voice kick from \"kick\"\nsource \"lib/b.blast\"\n",
    );
    // relative paths start at the sources folder, not at the sourcing file
    dir.write(
        "lib/b.blast",
        "voice hat from \"hat\"\nsource \"c.blast\"\n",
    );
    dir.write("c.blast", "voice snare from \"snare\"\n");
    let mut cp = lang_processor();
    cp.set_sources_dir(dir.0.clone());
    let (mut tx, mut rx) = cmd_queue(16);
    cp.run_script(&a, &mut tx).unwrap();
    for n in ["kick", "hat", "snare"] {
        assert!(cp.engine_state.voice_names.contains_key(n), "{n}");
    }
    // one batch per line, sourced files' lines included: kick, hat, snare
    assert_eq!(drain(&mut rx).len(), 3);

    // an error in a sourced file names that file
    dir.write("lib/d.blast", "voice x from \"nope\"\n");
    let top = dir.write("top.blast", "source \"lib/d.blast\"\n");
    let errs = lang_errs(cp.check_script(&top).unwrap_err());
    assert_eq!(codes(&errs), [Code::UnknownName]);
    assert!(errs[0].to_string().contains("d.blast:1:"), "{}", errs[0]);

    // a file that sources itself
    let me = dir.write("self.blast", "source \"self.blast\"\n");
    let errs = lang_errs(cp.check_script(&me).unwrap_err());
    assert_eq!(codes(&errs), [Code::SourceCycle]);

    // a cycle through two files
    dir.write("p.blast", "source \"q.blast\"\n");
    let q = dir.write("q.blast", "source \"p.blast\"\n");
    let errs = lang_errs(cp.check_script(&q).unwrap_err());
    assert_eq!(codes(&errs), [Code::SourceCycle]);

    // a chain: 16 files are fine, 17 are too deep
    for n in 0..17 {
        let text = if n == 16 {
            "# the end\n".to_string()
        } else {
            format!("source \"f{}.blast\"\n", n + 1)
        };
        dir.write(&format!("f{n}.blast"), &text);
    }
    let first = dir.0.join("f0.blast");
    let errs = lang_errs(cp.check_script(&first).unwrap_err());
    assert_eq!(codes(&errs), [Code::SourceDepth]);
    dir.write("f15.blast", "# the end\n");
    cp.check_script(&first).unwrap();

    // a missing file, from a script and from the prompt
    let m = dir.write("m.blast", "source \"nope.blast\"\n");
    let errs = lang_errs(cp.check_script(&m).unwrap_err());
    assert_eq!(codes(&errs), [Code::SourceNotFound]);
    assert_eq!(errs[0].span.line, 1);
    let errs = lerr(&mut cp, "source \"/no/such/file.blast\"");
    assert_eq!(codes(&errs), [Code::SourceNotFound]);
    let errs = lang_errs(
        cp.check_script(Path::new("/no/such/script.blast"))
            .unwrap_err(),
    );
    assert_eq!(codes(&errs), [Code::SourceNotFound]);
}

#[test]
fn source_at_the_prompt() {
    let dir = TempDir::new("prompt");
    let f = dir.write(
        "lib.blast",
        "voice kick from \"kick\"\nvoice hat from \"hat\"\n",
    );
    let mut cp = lang_processor();
    let cmd = ok(&mut cp, &format!("source \"{}\"", f.display()));
    assert!(matches!(cmd, Command::Batch(b) if b.cmds.len() == 2));
    assert_eq!(cp.engine_state.voices.len(), 2);

    // an error in the sourced file undoes the whole statement
    let bad = dir.write(
        "bad.blast",
        "voice snare from \"snare\"\nvoice x from \"nope\"\n",
    );
    let errs = lerr(&mut cp, &format!("source \"{}\"", bad.display()));
    assert_eq!(codes(&errs), [Code::UnknownName]);
    assert_eq!(cp.engine_state.voices.len(), 2);
}

#[test]
fn clear_in_source_does_nothing() {
    let dir = TempDir::new("clear");
    let f = dir.write("c.blast", "clear\nvoice kick from \"kick\"\n");
    let mut cp = lang_processor();
    ok(&mut cp, &format!("source \"{}\"", f.display()));
    assert!(!cp.clear_requested);
    // a file run with --script is on the stack too
    let mut cp = lang_processor();
    let (mut tx, _rx) = cmd_queue(16);
    cp.run_script_text(&f, &mut tx).unwrap();
    assert!(!cp.clear_requested);
}

// ---- state ----

const SESSION: &str = "\
clock main 124b
clock slow 500ms
voice kick from \"kick\"
voice hat from \"hat\" on 93b rate=0.5
voice bass from \"bass\" in 8
group drums { kick; hat }
kick { seq beat 1 2 3 4 }
kick { seq fill 1..2~0.1 4%50 late=0.1 }
hat { seq off 1.5 2.5 3.5 4.5%40 in 8 on slow }
bass { seq low 1 3 on 60b }";

const STATE: &str = "\
clock main 124b
clock slow 500ms
group drums {
  voice kick from \"kick\"
  voice hat from \"hat\" on 93b rate=0.5
}
voice bass from \"bass\" in 8
kick {
  seq beat 1 2 3 4
  seq fill 1~0.1 2~0.1 4%50 late=0.1
}
hat { seq off 1.5 2.5 3.5 4.5%40 in 8 on slow }
bass { seq low 1 3 on 60b }";

fn session() -> CmdProcessor {
    // not booted: the first statement makes `main`
    let mut cp = processor(&["kick", "hat", "snare", "bass"]);
    ok(&mut cp, SESSION);
    cp
}

#[test]
fn state_round_trip() {
    let mut cp = session();
    assert_eq!(cp.state(None).unwrap(), STATE);

    // feeding it back changes nothing: every statement is W101
    let text = cp.state(None).unwrap();
    let answer = local(&mut cp, &text);
    assert!(!answer.is_empty());
    for line in answer.lines() {
        assert!(line.contains("W101"), "{line}");
    }
    assert_eq!(cp.state(None).unwrap(), STATE);

    // `state` as a statement answers the same text
    assert_eq!(local(&mut cp, "state"), STATE);

    // and the text recreates the session from nothing
    let mut fresh = processor(&["kick", "hat", "snare", "bass"]);
    ok(&mut fresh, STATE);
    assert_eq!(fresh.state(None).unwrap(), STATE);
}

#[test]
fn state_of_an_idle_session_is_a_comment() {
    let mut cp = lang_processor();
    let text = cp.state(None).unwrap();
    assert!(text.starts_with('#'));
    assert_eq!(local(&mut cp, &text), "");
}

#[test]
fn state_one_object() {
    let mut cp = session();
    assert_eq!(
        cp.state(Some("drums")).unwrap(),
        "\
group drums {
  voice kick from \"kick\"
  voice hat from \"hat\" on 93b rate=0.5
}
kick {
  seq beat 1 2 3 4
  seq fill 1~0.1 2~0.1 4%50 late=0.1
}
hat { seq off 1.5 2.5 3.5 4.5%40 in 8 on slow }"
    );
    assert_eq!(cp.state(Some("main")).unwrap(), "clock main 124b");
    assert_eq!(
        cp.state(Some("bass")).unwrap(),
        "voice bass from \"bass\" in 8\nbass { seq low 1 3 on 60b }"
    );
    assert_eq!(
        cp.state(Some("kick.beat")).unwrap(),
        "kick { seq beat 1 2 3 4 }"
    );
    assert_eq!(
        local(&mut cp, "state drums.hat"),
        "voice hat from \"hat\" on 93b rate=0.5\nhat { seq off 1.5 2.5 3.5 4.5%40 in 8 on slow }"
    );
    let errs = lang_errs(cp.state(Some("drms")).unwrap_err());
    assert_eq!(codes(&errs), [Code::UnknownName]);
    assert_eq!(errs[0].suggestion.as_deref(), Some("did you mean 'drums'?"));
}

#[test]
fn state_with_a_command_in_the_line() {
    // a line that makes commands and asks a question: both are answered
    let mut cp = lang_processor();
    let (mut tx, mut rx) = cmd_queue(8);
    let text = cp
        .submit("voice kick from \"kick\"\nstate kick", &mut tx)
        .unwrap();
    assert_eq!(text.as_deref(), Some("voice kick from \"kick\""));
    assert!(matches!(drain(&mut rx)[..], [Command::Load(_)]));
}

// ---- help and the card ----

#[test]
fn help_lists_every_kind_and_action() {
    let mut cp = lang_processor();
    let text = local(&mut cp, "help");
    for k in kinds() {
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with(kind_word(k)))
            .unwrap_or_else(|| panic!("no line for {}", kind_word(k)));
        assert!(line.contains(schema(k).doc), "{line}");
    }
    for a in ACTIONS {
        assert!(
            text.lines().any(|l| l.trim_start().starts_with(a.word)),
            "{}",
            a.word
        );
    }
    // `wave` is built, so it doesn't say otherwise
    assert!(!text.contains("not built yet"));
}

#[test]
fn help_voice_names_every_setting() {
    let mut cp = lang_processor();
    let text = local(&mut cp, "help voice");
    for s in schema(Kind::Voice).settings {
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{} ", s.name)) && l.contains("default"))
            .unwrap_or_else(|| panic!("no line for {}", s.name));
        assert!(line.contains(s.doc), "{line}");
        assert_eq!(line.contains("not built yet"), s.built.is_some(), "{line}");
    }
    assert!(text.contains("from"));
    assert!(text.contains("glides"));
    // an action, a script word, a kind that isn't built
    assert!(local(&mut cp, "help start").contains("plays from the top"));
    assert!(local(&mut cp, "help let").starts_with("let:"));
    let wave = local(&mut cp, "help wave");
    assert!(wave.contains("shape") && wave.contains("retrig"), "{wave}");
    assert!(!wave.contains("not built yet"), "{wave}");

    // `gain` is a setting, not a topic: point at its kind
    let errs = lerr(&mut cp, "help gain");
    assert_eq!(codes(&errs), [Code::UnknownName]);
    assert!(
        errs[0]
            .suggestion
            .as_deref()
            .unwrap()
            .contains("'help voice'"),
        "{:?}",
        errs[0].suggestion
    );
    let errs = lerr(&mut cp, "help volce");
    assert_eq!(errs[0].suggestion.as_deref(), Some("did you mean 'voice'?"));
}

#[test]
fn card_is_small_and_valid() {
    let text = card();
    assert!(text.len() < 8000, "{} bytes", text.len());
    assert!(
        text.contains("Kinds: clock (rate), track (from; format"),
        "{text}"
    );
    // the line wraps, so the settings are checked apart from the relations
    assert!(text.contains("voice (from, on,"), "{text}");
    assert!(text.contains("gain pan rate pitch"), "{text}");

    // every example parses
    for (i, ex) in EXAMPLES.iter().enumerate() {
        assert!(text.contains(ex.title), "piece {}", i + 1);
        for stmt in ex.stmts {
            let src = Source {
                name: "card",
                text: stmt,
            };
            assert!(
                matches!(parse(src), Ok(Parse::Complete(_))),
                "piece {}: {stmt}",
                i + 1
            );
        }
    }

    // the ten pieces in one session, statement by statement: what is built
    // lowers (Ok), and the rest answers with the codes below. Later pieces
    // lean on earlier ones, so a statement whose voice never got made says
    // E301 as well.
    let mut cp = lang_processor();
    let mut got: Vec<(usize, String)> = Vec::new();
    for (i, ex) in EXAMPLES.iter().enumerate() {
        for stmt in ex.stmts {
            let outcome = match cp.parse(stmt) {
                Ok(_) => "ok".to_string(),
                Err(CmdErr::Lang(errs)) => errs
                    .iter()
                    .map(|e| e.code.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                Err(e) => panic!("piece {}: {stmt}: {e}", i + 1),
            };
            got.push((i + 1, outcome));
        }
    }
    let want: [(usize, &str); 19] = [
        (1, "ok"), // clock main 124b retimes main
        (1, "ok"),
        (1, "ok"),
        (1, "ok"),
        (2, "ok"),
        (2, "ok"),
        (3, "ok"),
        (3, "ok"),
        (4, "ok"),
        (5, "ok"),
        (5, "ok"),
        (6, "ok"),
        (6, "ok"),
        (7, "ok"),
        (8, "ok"), // a voice of waves needs no `from`
        (8, "ok"),
        (9, "ok"),
        (10, "E301"), // `lead` was never made in this session
        (10, "ok"),
    ];
    assert_eq!(got, want.map(|(p, o)| (p, o.to_string())));
}

// the steps of every seq of `kick`, as (at, chance, early, late)
fn kick_steps(cp: &CmdProcessor, seq: &str) -> Vec<(f32, f32, f32, f32)> {
    let es = &cp.engine_state;
    let voice = &es.voices[&es.voice_id("kick").unwrap()];
    es.procs[&voice.procs[seq]]
        .seq
        .steps
        .iter()
        .map(|s| (s.at, s.chance, s.early, s.late))
        .collect()
}

#[test]
fn state_round_trips_window() {
    let mut cp = lang_processor();
    ok(&mut cp, "voice kick from \"kick\" shift=spectral pitch=2st");
    // the default window is not printed
    assert!(!cp.state(None).unwrap().contains("window"));
    ok(&mut cp, "kick w=60ms");
    let text = cp.state(None).unwrap();
    assert!(text.contains("window=60ms"), "{text}");
    // feeding it back changes nothing
    for line in local(&mut cp, &text).lines() {
        assert!(line.contains("W101"), "{line}");
    }
    // and recreates the voice from nothing
    let mut fresh = lang_processor();
    ok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    // spectral is printed (grains, the default, is not) and survives
    assert!(text.contains("shift=spectral"), "{text}");
    let shift = |cp: &CmdProcessor| cp.engine_state.voices.values().next().unwrap().shift;
    assert_eq!(shift(&fresh), ShiftMode::Spectral);
    let mut plain = lang_processor();
    ok(&mut plain, "voice kick from \"kick\" pitch=2st");
    assert!(!plain.state(None).unwrap().contains("shift"));
    assert_eq!(shift(&plain), ShiftMode::Grains);
    // back at the default, it is not printed again
    ok(&mut cp, "kick window=90ms");
    assert!(!cp.state(None).unwrap().contains("window"));
}

#[test]
fn state_round_trips_bounce() {
    let mut cp = lang_processor();
    ok(&mut cp, "voice kick from \"kick\" loop=pingpong");
    // the default bounce is not printed
    assert!(!cp.state(None).unwrap().contains("bounce"));
    ok(&mut cp, "kick u=1ms");
    let text = cp.state(None).unwrap();
    assert!(text.contains("bounce=1ms"), "{text}");
    for line in local(&mut cp, &text).lines() {
        assert!(line.contains("W101"), "{line}");
    }
    let mut fresh = lang_processor();
    ok(&mut fresh, &text);
    assert_eq!(fresh.state(None).unwrap(), text);
    // off is printed, and the default is not again
    ok(&mut cp, "kick bounce=0");
    assert!(cp.state(None).unwrap().contains("bounce=0ms"));
    ok(&mut cp, "kick bounce=2ms");
    assert!(!cp.state(None).unwrap().contains("bounce"));
}

#[test]
fn state_round_trips_feel() {
    // (written, the seq line `state` prints)
    let cases = [
        // a symmetric base, with a `~` override (and `~0`)
        ("1 2 3 4~0 jitter=0.1", "1 2 3 4~0 jitter=0.1"),
        ("1~0.2 2 3 4 jitter=0.1", "1~0.2 2 3 4 jitter=0.1"),
        // a one-sided base, with `~0` under it
        ("1 2~0 late=0.04", "1 2~0 late=0.04"),
        ("1 2 early=0.02", "1 2 early=0.02"),
        ("1 2 early=0.02 late=0.05", "1 2 early=0.02 late=0.05"),
        // a tie goes to the earliest step
        ("1~0.1 2", "1 2~0 jitter=0.1"),
        ("1 2~0.1", "1 2~0.1"),
        // chance: one `chance=` when all agree, else `%` on the steps
        ("1 2 chance=70", "1 2 chance=70"),
        ("1 2%35 chance=70", "1%70 2%35"),
        ("2%40", "2%40"),
        ("1 2 chance=70 late=0.1", "1 2 chance=70 late=0.1"),
    ];
    for (written, printed) in cases {
        let mut cp = lang_processor();
        ok(&mut cp, "voice kick from \"kick\"");
        ok(&mut cp, &format!("kick {{ seq s {written} }}"));
        let text = cp.state(Some("kick")).unwrap();
        assert!(
            text.contains(&format!("seq s {printed}")),
            "{written}: {text}"
        );
        // pasting it into a fresh session makes the same steps
        let mut fresh = lang_processor();
        ok(&mut fresh, &text);
        assert_eq!(
            kick_steps(&fresh, "s"),
            kick_steps(&cp, "s"),
            "{written}: {text}"
        );
        assert_eq!(fresh.state(Some("kick")).unwrap(), text, "{written}");
    }
}

#[test]
fn help_shows_short_forms() {
    let mut cp = lang_processor();
    let text = local(&mut cp, "help voice");
    assert!(text.contains("rate (r)"), "{text}");
    let text = local(&mut cp, "help seq");
    assert!(text.contains("late (l)"), "{text}");
}

// ---- sources folder (LANGf) ----

/// A processor whose sources folder is `<dir>/sources`.
fn with_sources(dir: &TempDir) -> CmdProcessor {
    let mut cp = lang_processor();
    cp.set_sources_dir(dir.0.join("sources"));
    cp
}

#[test]
fn source_by_name() {
    let dir = TempDir::new("byname");
    dir.write("sources/kit.blast", "voice kick from \"kick\"\n");
    for line in ["source kit", "source kit.blast", "source \"kit.blast\""] {
        let mut cp = with_sources(&dir);
        ok(&mut cp, line);
        assert!(cp.engine_state.voice_names.contains_key("kick"), "{line}");
    }
    // a name with dashes and slashes, and a file with no extension
    dir.write("sources/lib/set-1.blast", "voice hat from \"hat\"\n");
    dir.write("sources/plain", "voice snare from \"snare\"\n");
    let mut cp = with_sources(&dir);
    ok(&mut cp, "source lib/set-1");
    ok(&mut cp, "source plain");
    assert_eq!(cp.engine_state.voices.len(), 2);
    let errs = lerr(&mut with_sources(&dir), "source missing");
    assert_eq!(codes(&errs), [Code::SourceNotFound]);
}

#[test]
fn source_nested_resolves_from_sources_dir() {
    let dir = TempDir::new("nested");
    dir.write("sources/lib/a.blast", "source b\n");
    dir.write("sources/b.blast", "voice kick from \"kick\"\n");
    dir.write("sources/lib/b.blast", "voice hat from \"hat\"\n");
    let mut cp = with_sources(&dir);
    ok(&mut cp, "source lib/a");
    assert!(cp.engine_state.voice_names.contains_key("kick"));
    assert!(!cp.engine_state.voice_names.contains_key("hat"));
}

#[test]
fn source_quoted_absolute_and_home() {
    let dir = TempDir::new("quoted");
    let abs = dir.write("elsewhere/x.blast", "voice kick from \"kick\"\n");
    let mut cp = with_sources(&dir);
    ok(&mut cp, &format!("source \"{}\"", abs.display()));
    assert!(cp.engine_state.voice_names.contains_key("kick"));

    // `~/` is $HOME (given here, as blast has no unsafe to set the variable)
    dir.write("home/h.blast", "voice hat from \"hat\"\n");
    let q = super::ast::SourcePath::Quoted("~/h.blast".into(), Default::default());
    let home = Some(dir.0.join("home").into_os_string());
    let got = cp.join_source_with(&q, home);
    assert_eq!(got, dir.0.join("home/h.blast"));
    // no HOME: the path is kept, under the sources folder
    assert_eq!(
        cp.join_source_with(&q, None),
        cp.sources_dir.join("~/h.blast")
    );
}

#[test]
fn source_read_prints_without_running() {
    let dir = TempDir::new("read");
    dir.write("sources/kit.blast", "voice kick from \"kick\"\n");
    for line in ["source kit read", "source kit r"] {
        let mut cp = with_sources(&dir);
        assert_eq!(local(&mut cp, line), "voice kick from \"kick\"", "{line}");
        assert!(cp.engine_state.voices.is_empty());
    }
    let errs = lerr(&mut with_sources(&dir), "source nope read");
    assert_eq!(codes(&errs), [Code::SourceNotFound]);
}

const HIST_FILE: &str = "\
# a header comment

voice kick from \"kick\"

voice hat from \"hat\" # trailing
kick {
  # inside
  gain 0.5
  pan -0.2
}
group drums { kick; hat }; clear

# the end
";

fn history_of(cp: &mut CmdProcessor, line: &str) -> (String, Vec<String>) {
    let text = local(cp, line);
    (text, std::mem::take(&mut cp.history_out))
}

#[test]
fn source_history_adds_statements_in_order() {
    let dir = TempDir::new("history");
    dir.write("sources/kit.blast", HIST_FILE);
    for line in ["source kit history", "source kit h"] {
        let mut cp = with_sources(&dir);
        let (text, got) = history_of(&mut cp, line);
        assert_eq!(text, "history: 5 statements from kit.blast", "{line}");
        assert_eq!(
            got,
            [
                "voice kick from \"kick\"",
                "voice hat from \"hat\" # trailing",
                "kick {\n  # inside\n  gain 0.5\n  pan -0.2\n}",
                "group drums { kick; hat }",
                "clear",
            ],
            "{line}"
        );
    }
}

#[test]
fn source_read_history_prints_and_adds() {
    let dir = TempDir::new("readhist");
    dir.write("sources/kit.blast", "voice kick from \"kick\"\n");
    for line in ["source kit read history", "source kit h r"] {
        let mut cp = with_sources(&dir);
        let (text, got) = history_of(&mut cp, line);
        assert_eq!(
            text, "voice kick from \"kick\"\nhistory: 1 statement from kit.blast",
            "{line}"
        );
        assert_eq!(got, ["voice kick from \"kick\""]);
    }
}

#[test]
fn source_history_missing_e701() {
    let dir = TempDir::new("histmissing");
    for line in ["source nope history", "source nope read history"] {
        let mut cp = with_sources(&dir);
        let errs = lerr(&mut cp, line);
        assert_eq!(codes(&errs), [Code::SourceNotFound], "{line}");
        assert!(cp.history_out.is_empty());
    }
}

#[test]
fn source_history_skips_comments_and_blanks() {
    let dir = TempDir::new("histcomments");
    dir.write("sources/c.blast", "# only\n\n   \n# comments\n");
    let mut cp = with_sources(&dir);
    let (text, got) = history_of(&mut cp, "source c history");
    assert_eq!(text, "history: 0 statements from c.blast");
    assert!(got.is_empty());
}

#[test]
fn source_history_does_not_run() {
    let dir = TempDir::new("histnorun");
    dir.write("sources/kit.blast", HIST_FILE);
    let mut cp = with_sources(&dir);
    let (_, first) = history_of(&mut cp, "source kit history");
    assert_eq!(first.len(), 5);
    assert!(cp.engine_state.voices.is_empty());
    assert!(!cp.clear_requested);
    // the file isn't checked: a name that doesn't exist is fine
    dir.write("sources/odd.blast", "nosuch gain=1\n");
    let (_, got) = history_of(&mut cp, "source odd history");
    assert_eq!(got, ["nosuch gain=1"]);
}

#[test]
fn state_to_writes_and_round_trips() {
    let dir = TempDir::new("stateto");
    let mut cp = session();
    cp.set_sources_dir(dir.0.join("sources"));
    let said = local(&mut cp, "state to set1");
    assert_eq!(said, "wrote sources/set1.blast");
    let text = std::fs::read_to_string(dir.0.join("sources/set1.blast")).unwrap();
    assert_eq!(text, cp.state(None).unwrap());

    let mut fresh = processor(&["kick", "hat", "snare", "bass"]);
    fresh.set_sources_dir(dir.0.join("sources"));
    ok(&mut fresh, "source set1");
    assert_eq!(fresh.state(None).unwrap(), cp.state(None).unwrap());

    // one object, into a sub-folder that doesn't exist yet
    local(&mut cp, "state drums to lib/drums");
    let text = std::fs::read_to_string(dir.0.join("sources/lib/drums.blast")).unwrap();
    assert_eq!(text, cp.state(Some("drums")).unwrap());
}

#[test]
fn state_to_replaced() {
    let dir = TempDir::new("replaced");
    let mut cp = session();
    cp.set_sources_dir(dir.0.join("sources"));
    assert_eq!(local(&mut cp, "state to x"), "wrote sources/x.blast");
    assert_eq!(
        local(&mut cp, "state to x"),
        "wrote sources/x.blast (replaced)"
    );
}

#[test]
fn state_to_in_a_script_writes_and_failure_is_e704() {
    let dir = TempDir::new("script-write");
    let f = dir.write("w.blast", "voice kick from \"kick\"\nstate to out\n");
    let mut cp = with_sources(&dir);
    let (mut tx, _rx) = cmd_queue(16);
    let text = cp.run_script_text(&f, &mut tx).unwrap();
    assert_eq!(text, "wrote sources/out.blast");
    assert!(dir.0.join("sources/out.blast").exists());

    // a folder in the way: the write fails, E704, nothing is rolled back
    dir.write("sources/blocked.blast/keep", "");
    let errs = lerr(&mut cp, "state to blocked");
    assert_eq!(codes(&errs), [Code::SourceWrite]);
    // with commands in the line they must still reach the engine, so the
    // error is printed with the answers instead
    ok(&mut cp, "voice hat from \"hat\" and state to blocked");
    assert!(cp.outbox.contains("E704"), "{}", cp.outbox);
    assert!(cp.engine_state.voice_names.contains_key("hat"));
}

#[test]
fn check_never_writes() {
    let dir = TempDir::new("check-write");
    let f = dir.write("c.blast", "voice kick from \"kick\"\nstate to x\n");
    let mut cp = with_sources(&dir);
    cp.check_script(&f).unwrap();
    assert_eq!(local(&mut cp, &format!("check \"{}\"", f.display())), "ok");
    assert!(!dir.0.join("sources").exists());
}

#[test]
fn failed_line_never_writes() {
    let dir = TempDir::new("failed-write");
    let mut cp = with_sources(&dir);
    let errs = lerr(&mut cp, "state to x and voice y from \"nope\"");
    assert_eq!(codes(&errs), [Code::UnknownName]);
    assert!(!dir.0.join("sources").exists());
}

#[test]
fn script_arg_resolution() {
    let dir = TempDir::new("scriptarg");
    dir.write("sources/phase.blast", "voice kick from \"kick\"\n");
    let cp = with_sources(&dir);
    // a name is looked up in the sources folder
    assert_eq!(
        cp.resolve_script_arg(Path::new("phase")),
        dir.0.join("sources/phase.blast")
    );
    // a file that exists as given wins
    let local_file = dir.write("here.blast", "# x\n");
    assert_eq!(cp.resolve_script_arg(&local_file), local_file);
}

#[test]
fn state_prints_generator_and_round_trips() {
    for (written, printed) in [
        ("steps=3 in 8", "seq s steps=3 in 8"),
        ("steps=3 rot=1 in 8", "seq s steps=3 rot=1 in 8"),
        ("n=5 g=16 r=-2 in 8", "seq s steps=5 grid=16 rot=-2 in 8"),
        ("steps=1 chance=50", "seq s steps=1 chance=50"),
        ("steps=2 jitter=0.1", "seq s steps=2 jitter=0.1"),
    ] {
        let mut cp = lang_processor();
        ok(&mut cp, "voice kick from \"kick\"");
        ok(&mut cp, &format!("kick {{ seq s {written} }}"));
        let text = cp.state(Some("kick")).unwrap();
        assert!(text.contains(printed), "{written}: {text}");
        // pasting it into a fresh session makes the same seq
        let mut fresh = lang_processor();
        ok(&mut fresh, &text);
        assert_eq!(kick_steps(&fresh, "s"), kick_steps(&cp, "s"), "{text}");
        assert_eq!(fresh.state(Some("kick")).unwrap(), text, "{written}");
    }
}

// ---- shifters on every path (SHFIX) ----

// the voices that `SetShifters` commands in `cmd` (or in its batches and
// schedules) are for
fn shifted(cmd: &Command) -> Vec<VoiceId> {
    match cmd {
        Command::SetShifters(a) => vec![a.voice],
        Command::Batch(b) => b.cmds.iter().flat_map(shifted).collect(),
        Command::Schedule(s) => shifted(&s.cmd),
        _ => vec![],
    }
}

const SPECTRAL: &str = "voice v from \"kick\" shift=spectral pitch=3st\n";
const GRAINS: &str = "voice v from \"kick\" shift=grains pitch=3st\n";
const DEFAULT: &str = "voice v from \"kick\" pitch=3st\n";

#[test]
fn shifters_from_typed_input() {
    let mut cp = lang_processor();
    let cmd = ok(&mut cp, SPECTRAL.trim());
    assert_eq!(shifted(&cmd).len(), 1);
    for line in [GRAINS, DEFAULT] {
        let mut cp = lang_processor();
        assert!(shifted(&ok(&mut cp, line.trim())).is_empty(), "{line}");
    }
}

#[test]
fn shifters_from_source_at_the_prompt() {
    let dir = TempDir::new("shifters-source");
    let f = dir.write("s.blast", SPECTRAL);
    let mut cp = lang_processor();
    let cmd = ok(&mut cp, &format!("source \"{}\"", f.display()));
    assert_eq!(shifted(&cmd).len(), 1);
    for (name, text) in [("g.blast", GRAINS), ("d.blast", DEFAULT)] {
        let f = dir.write(name, text);
        let mut cp = lang_processor();
        let cmd = ok(&mut cp, &format!("source \"{}\"", f.display()));
        assert!(shifted(&cmd).is_empty(), "{name}");
    }
}

#[test]
fn shifters_from_apply_lines() {
    // `--script` and `--export` run a file line by line
    let dir = TempDir::new("shifters-apply");
    for (name, text, want) in [("s", SPECTRAL, 1), ("g", GRAINS, 0), ("d", DEFAULT, 0)] {
        let f = dir.write(&format!("{name}.blast"), text);
        let mut cp = lang_processor();
        let (mut tx, mut rx) = cmd_queue(16);
        cp.run_script(&f, &mut tx).unwrap();
        let n: usize = drain(&mut rx).iter().map(|c| shifted(c).len()).sum();
        assert_eq!(n, want, "{name}");
    }
    // a later line that makes an existing voice want them
    let f = dir.write(
        "l.blast",
        "voice v from \"kick\" shift=spectral\nv pitch=3st\n",
    );
    let mut cp = lang_processor();
    let (mut tx, mut rx) = cmd_queue(16);
    cp.run_script(&f, &mut tx).unwrap();
    let n: usize = drain(&mut rx).iter().map(|c| shifted(c).len()).sum();
    assert_eq!(n, 1);
}
