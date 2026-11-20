//! Completions and names for the REPL (INS, README §3.8).
//!
//! `completions` lexes the line up to the cursor with LANG's lexer (errors
//! are ignored), finds the word being completed and what comes before it,
//! and offers the words the schema, the keywords and the session's names
//! allow there. Everything is built from LANG's tables, so a new setting or
//! kind completes with no change here. Candidates replace the word being
//! completed; a word ends at whitespace and at `, ; { } | = .` (REQ-062
//! H7), so `loop=` offers `off forward pingpong` and `kick.` offers `hat`.
//! Quoted words keep their opening quote (`"kick"`, `"~/sounds/`).

use std::ffi::OsString;
use std::path::Path;

use super::CmdProcessor;
use super::lang::ast::{ActionWord, Ident, Kind, SourcePath};
use super::lang::errors::Span;
use super::lang::help::{OTHER_WORDS, built_settings, kinds, relations};
use super::lang::lexer::{Kw, Tok, Token, lex};
use super::lang::names::{Named, all_names, lookup};
use super::lang::schema::{ACTIONS, BlockUse, SettingType, kind_word, schema};
use super::reprs::{EngineState, ProcOwner};

/// Which names `CmdProcessor::names` lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameKind {
    Clock,
    Voice,
    Group,
    /// Seqs as written: `name` at the top level, `owner.name` otherwise.
    Seq,
    /// Packets, written like seqs.
    Packet,
    /// Tracks (written quoted in the language).
    Track,
    /// Clocks, voices, groups, seqs and packets (no tracks).
    Any,
}

/// The most entries read from one folder.
const MAX_ENTRIES: usize = 1000;

impl CmdProcessor {
    /// The words that could replace the word touching `cursor` (a byte
    /// offset into `line`), sorted and without duplicates. Reads the
    /// sources folder (and `$HOME` for `~/`) when a path is being written.
    pub fn completions(&self, line: &str, cursor: usize) -> Vec<String> {
        self.completions_with_home(line, cursor, std::env::var_os("HOME"))
    }

    /// The names of one kind in the session, sorted.
    pub fn names(&self, kind: NameKind) -> Vec<String> {
        let es = &self.engine_state;
        let mut out: Vec<String> = match kind {
            NameKind::Clock => es.context_names.keys().cloned().collect(),
            NameKind::Voice => es.voices.values().map(|v| v.name.clone()).collect(),
            NameKind::Group => es.group_names.keys().cloned().collect(),
            NameKind::Seq => es
                .procs
                .values()
                .map(|p| owned_name(es, p.owner, &p.name))
                .collect(),
            NameKind::Packet => es
                .packets
                .values()
                .map(|p| owned_name(es, p.owner, &p.name))
                .collect(),
            NameKind::Track => es.tracks.keys().cloned().collect(),
            NameKind::Any => [
                NameKind::Clock,
                NameKind::Voice,
                NameKind::Group,
                NameKind::Seq,
                NameKind::Packet,
            ]
            .into_iter()
            .flat_map(|k| self.names(k))
            .collect(),
        };
        out.sort();
        out.dedup();
        out
    }

    // `completions` with `$HOME` as a parameter, so tests can use `~/`
    // (blast has no unsafe to set the variable)
    pub(crate) fn completions_with_home(
        &self,
        line: &str,
        cursor: usize,
        home: Option<OsString>,
    ) -> Vec<String> {
        let mut end = cursor.min(line.len());
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        let mut out = self.candidates(&line[..end], home);
        out.sort();
        out.dedup();
        out
    }

    fn candidates(&self, text: &str, home: Option<OsString>) -> Vec<String> {
        // only the last line matters
        let text = text.rsplit('\n').next().unwrap_or(text);
        let (start, quoted) = match scan_tail(text) {
            Tail::Comment => return Vec::new(),
            Tail::Quoted(at) => (at, true),
            Tail::Plain => (word_start(text), false),
        };
        let word = &text[start..];
        let before = &text[..start];
        let st = statement(before);

        let all: Vec<String> = if quoted {
            self.quoted(&st, &word[1..], home)
        } else if let Some(owner) = before.strip_suffix('.').filter(|b| !b.ends_with('.')) {
            self.owner_children(owner)
        } else {
            self.unquoted(&st, home, word)
        };
        all.into_iter().filter(|c| c.starts_with(word)).collect()
    }

    // after an opening quote; `typed` is what follows it
    fn quoted(&self, st: &[Token], typed: &str, home: Option<OsString>) -> Vec<String> {
        let first = st.first().map(|t| &t.kind);
        let last = st.last().map(|t| &t.kind);
        let tracks = match last {
            None | Some(Tok::Act(ActionWord::Remove)) => true,
            Some(Tok::Kw(Kw::From)) => self.object_kind(st) != Some(Kind::Track),
            Some(Tok::Comma) => matches!(first, Some(Tok::Act(ActionWord::Remove))),
            _ => false,
        };
        if tracks {
            return self.track_words();
        }
        let to = matches!(last, Some(Tok::Name(n)) if n == "to")
            && matches!(first, Some(Tok::Kw(Kw::State)));
        if !(matches!(last, Some(Tok::Kw(Kw::Source | Kw::Check))) || to) {
            return Vec::new();
        }
        if typed == "~" {
            return vec!["\"~/".to_string()];
        }
        self.path_entries(typed, true, home)
            .into_iter()
            .map(|p| format!("\"{p}"))
            .collect()
    }

    // the candidates after `st`, before they are filtered by the word
    fn unquoted(&self, st: &[Token], home: Option<OsString>, word: &str) -> Vec<String> {
        let names = |ks: &[NameKind]| -> Vec<String> {
            ks.iter().flat_map(|k| self.top_names(*k)).collect()
        };
        let words = |ws: &[&str]| -> Vec<String> { ws.iter().map(|w| w.to_string()).collect() };
        let first = st.first().map(|t| &t.kind);
        let Some(last) = st.last().map(|t| &t.kind) else {
            return self.statement_starts();
        };

        // `at N`, then `by`, then something with a clock
        if matches!(last, Tok::Kw(Kw::By)) {
            return names(&[
                NameKind::Clock,
                NameKind::Group,
                NameKind::Voice,
                NameKind::Seq,
                NameKind::Packet,
            ]);
        }
        if matches!(last, Tok::Num(_) | Tok::Var(_))
            && st.len() >= 2
            && matches!(st[st.len() - 2].kind, Tok::Kw(Kw::At | Kw::After))
        {
            return words(&["by"]);
        }
        if matches!(last, Tok::Kw(Kw::At | Kw::After)) {
            return Vec::new();
        }

        // receivers after a pipe
        if st.iter().any(|t| t.kind == Tok::Pipe) {
            return match last {
                Tok::Pipe | Tok::Comma => words(&["trig", "melo"]),
                Tok::Recv(_) => names(&[NameKind::Voice, NameKind::Group]),
                _ => Vec::new(),
            };
        }

        match first {
            Some(Tok::Act(a)) => {
                return match last {
                    Tok::Act(_) => self.action_targets(*a, true),
                    Tok::Comma => self.action_targets(*a, false),
                    _ => Vec::new(),
                };
            }
            Some(Tok::Kw(Kw::Source | Kw::Check)) => {
                return match (st.len(), last) {
                    (1, _) => self.path_entries(word, false, home),
                    (2, Tok::Name(_)) if first == Some(&Tok::Kw(Kw::Source)) => {
                        words(&["read", "history"])
                    }
                    // the two words come in any order, once each
                    (3, Tok::Name(n)) if first == Some(&Tok::Kw(Kw::Source)) => match n.as_str() {
                        "read" | "r" => words(&["history"]),
                        "history" | "h" => words(&["read"]),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
            }
            Some(Tok::Kw(Kw::State)) => {
                return match last {
                    Tok::Kw(Kw::State) => {
                        let mut v: Vec<String> = all_names(&self.engine_state)
                            .into_iter()
                            .map(String::from)
                            .collect();
                        v.push("to".to_string());
                        v
                    }
                    Tok::Name(n) if n == "to" => self.path_entries(word, false, home),
                    Tok::Name(_) => words(&["to"]),
                    _ => Vec::new(),
                };
            }
            Some(Tok::Kw(Kw::Help)) => {
                return if st.len() == 1 {
                    help_words()
                } else {
                    Vec::new()
                };
            }
            _ => {}
        }

        match last {
            Tok::Kw(Kw::On) => names(&[NameKind::Clock, NameKind::Group, NameKind::Voice]),
            Tok::Kw(Kw::From) if self.object_kind(st) != Some(Kind::Track) => self.track_words(),
            Tok::Eq => self.setting_words(st),
            Tok::Name(_)
            | Tok::Num(_)
            | Tok::Dur(..)
            | Tok::Db(_)
            | Tok::Pitch(..)
            | Tok::Str(_)
            | Tok::Var(_)
            | Tok::RParen
            | Tok::Kw(Kw::Yes | Kw::No | Kw::All) => self.item_words(st),
            _ => Vec::new(),
        }
    }

    // kinds, actions, script words and the session's names
    fn statement_starts(&self) -> Vec<String> {
        let mut out: Vec<String> = kinds()
            .filter(|k| schema(*k).built.is_none())
            .map(|k| kind_word(k).to_string())
            .collect();
        out.extend(
            ACTIONS
                .iter()
                .filter(|a| a.built.is_none())
                .map(|a| a.word.to_string()),
        );
        out.extend(OTHER_WORDS.iter().map(|(w, _)| w.to_string()));
        out.extend(all_names(&self.engine_state).into_iter().map(String::from));
        out
    }

    // what an action takes
    fn action_targets(&self, action: ActionWord, with_all: bool) -> Vec<String> {
        use NameKind::*;
        let (kinds, tracks): (&[NameKind], bool) = match action {
            ActionWord::Rescan => return Vec::new(),
            ActionWord::Ungroup => (&[Group, Voice], false),
            ActionWord::Remove => (&[Clock, Voice, Group, Seq, Packet], true),
            _ => (&[Clock, Voice, Group, Seq, Packet], false),
        };
        let mut out: Vec<String> = kinds.iter().flat_map(|k| self.top_names(*k)).collect();
        if tracks {
            out.extend(self.track_words());
        }
        if with_all {
            out.push("all".to_string());
        }
        out
    }

    // the value words of the setting before `=`
    fn setting_words(&self, st: &[Token]) -> Vec<String> {
        let (Some(kind), Some(Tok::Name(n))) = (
            self.object_kind(st),
            st.len().checked_sub(2).map(|i| &st[i].kind),
        ) else {
            return Vec::new();
        };
        let Some(spec) = built_settings(schema(kind)).find(|s| setting_is(s.name, s.short, n))
        else {
            return Vec::new();
        };
        match spec.ty {
            SettingType::Word(words) => words.iter().map(|w| w.to_string()).collect(),
            SettingType::NumberOr { words, .. } => words.iter().map(|w| w.to_string()).collect(),
            SettingType::Bool => vec!["yes".to_string(), "no".to_string()],
            _ => Vec::new(),
        }
    }

    // after an object's name or any of its items: settings, relations,
    // a block, a pipe
    fn item_words(&self, st: &[Token]) -> Vec<String> {
        let Some(kind) = self.object_kind(st) else {
            return Vec::new();
        };
        // a lone kind word waits for a name
        if st.len() < 2 && matches!(st.first().map(|t| &t.kind), Some(Tok::Kind(_))) {
            return Vec::new();
        }
        let sc = schema(kind);
        let given: Vec<&str> = st
            .windows(2)
            .filter_map(|w| match (&w[0].kind, &w[1].kind) {
                (Tok::Name(n), Tok::Eq) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        let has = |word: &str| st.iter().any(|t| t.kind.keyword_text() == Some(word));
        let mut out = Vec::new();
        for s in built_settings(sc) {
            if !given.iter().any(|g| setting_is(s.name, s.short, g)) {
                out.push(format!("{}=", s.name));
                out.push(format!("{}=", s.short));
            }
        }
        out.extend(
            relations(sc)
                .into_iter()
                .filter(|r| !has(r))
                .map(String::from),
        );
        if sc.block != BlockUse::No {
            out.push("{".to_string());
        }
        if sc.pipe {
            out.push("|".to_string());
        }
        out
    }

    // the kind of the statement `st` is about
    fn object_kind(&self, st: &[Token]) -> Option<Kind> {
        match &st.first()?.kind {
            Tok::Kind(k) => Some(*k),
            Tok::Str(_) => Some(Kind::Track),
            Tok::Name(_) => {
                let path = leading_path(st);
                lookup(&self.engine_state, &path).ok().map(Named::kind)
            }
            _ => None,
        }
    }

    // names written at the top level (a candidate word never holds a dot)
    fn top_names(&self, kind: NameKind) -> Vec<String> {
        let es = &self.engine_state;
        match kind {
            NameKind::Seq => es.session_procs.keys().cloned().collect(),
            NameKind::Packet => es
                .packets
                .values()
                .filter(|p| p.owner == ProcOwner::Session)
                .map(|p| p.name.clone())
                .collect(),
            other => self.names(other),
        }
    }

    fn track_words(&self) -> Vec<String> {
        self.names(NameKind::Track)
            .into_iter()
            .map(|t| format!("\"{t}\""))
            .collect()
    }

    // The entries of the folder `typed` points into, as `typed` continues:
    // quoted paths (`~/` is `home`, relative ones start at the sources
    // folder) or bare source names (`.blast` stripped).
    fn path_entries(&self, typed: &str, quoted: bool, home: Option<OsString>) -> Vec<String> {
        let (dir_part, prefix) = typed.split_at(typed.rfind('/').map_or(0, |i| i + 1));
        let written = if quoted {
            SourcePath::Quoted(dir_part.to_string(), Span::default())
        } else {
            SourcePath::Name(dir_part.to_string(), Span::default())
        };
        let dir = self.join_source_with(&written, home);
        read_entries(&dir, prefix, MAX_ENTRIES)
            .into_iter()
            .filter_map(|(name, is_dir)| {
                if is_dir {
                    return Some(format!("{dir_part}{name}/"));
                }
                if quoted {
                    return Some(format!("{dir_part}{name}\""));
                }
                let bare = name.strip_suffix(".blast").unwrap_or(&name);
                let plain = bare
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
                plain.then(|| format!("{dir_part}{bare}"))
            })
            .collect()
    }

    // the names under `owner` (`kick` in `kick.`, `drums.kick` in `drums.kick.`)
    fn owner_children(&self, owner: &str) -> Vec<String> {
        let es = &self.engine_state;
        // the owner is the whole trailing run of name characters and dots
        let start = owner
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))
            .last()
            .map_or(owner.len(), |(i, _)| i);
        let segs: Vec<&str> = owner[start..].split('.').collect();
        let well_formed = |s: &&str| {
            !s.is_empty()
                && !s.starts_with(|c: char| c.is_ascii_digit())
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if segs.len() > 3 || !segs.iter().all(well_formed) {
            return Vec::new();
        }
        let path: Vec<Ident> = segs
            .iter()
            .map(|s| Ident {
                name: s.to_string(),
                span: Span::default(),
            })
            .collect();
        let packets = |o: ProcOwner| -> Vec<String> {
            es.owner_packets(o)
                .into_iter()
                .map(|p| p.name.clone())
                .collect()
        };
        match lookup(es, &path) {
            Ok(Named::Voice(v)) => {
                let mut out: Vec<String> = es
                    .voices
                    .get(&v)
                    .map(|r| r.procs.keys().cloned().collect())
                    .unwrap_or_default();
                out.extend(packets(ProcOwner::Voice(v)));
                if let Some(r) = es.voices.get(&v) {
                    out.extend(r.fx.iter().map(|f| f.name.clone()));
                    out.extend(r.waves.iter().map(|w| w.name.clone()));
                }
                out
            }
            Ok(Named::Group(g)) => {
                let mut out = Vec::new();
                if let Some(r) = es.groups.get(&g) {
                    out.extend(r.member_names.keys().cloned());
                    out.extend(r.procs.keys().cloned());
                    out.extend(r.fx.iter().map(|f| f.name.clone()));
                }
                out.extend(packets(ProcOwner::Group(g)));
                out
            }
            _ => Vec::new(),
        }
    }
}

// `owner.name`, or `name` at the top level
fn owned_name(es: &EngineState, owner: ProcOwner, name: &str) -> String {
    let owner = match owner {
        ProcOwner::Session => return name.to_string(),
        ProcOwner::Voice(v) => es.voices.get(&v).map(|r| r.name.as_str()),
        ProcOwner::Group(g) => es.groups.get(&g).map(|r| r.name.as_str()),
    };
    match owner {
        Some(o) => format!("{o}.{name}"),
        None => name.to_string(),
    }
}

fn setting_is(name: &str, short: char, written: &str) -> bool {
    name == written || written.chars().eq(std::iter::once(short))
}

fn help_words() -> Vec<String> {
    let mut out: Vec<String> = kinds().map(|k| kind_word(k).to_string()).collect();
    out.extend(ACTIONS.iter().map(|a| a.word.to_string()));
    out.extend(OTHER_WORDS.iter().map(|(w, _)| w.to_string()));
    out
}

// (name, is a folder) of the entries of `dir` that start with `prefix`;
// hidden ones only when asked for with a dot
fn read_entries(dir: &Path, prefix: &str, max: usize) -> Vec<(String, bool)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let hidden = prefix.starts_with('.');
    rd.flatten()
        .take(max)
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if !name.starts_with(prefix) || (name.starts_with('.') && !hidden) {
                return None;
            }
            Some((name, e.path().is_dir()))
        })
        .collect()
}

// the leading `a`, `a.b` or `a.b.c` of a statement
fn leading_path(st: &[Token]) -> Vec<Ident> {
    let mut path = Vec::new();
    let mut i = 0;
    while let Some(Tok::Name(n)) = st.get(i).map(|t| &t.kind) {
        path.push(Ident {
            name: n.clone(),
            span: st[i].span,
        });
        if path.len() == 3 || !matches!(st.get(i + 1).map(|t| &t.kind), Some(Tok::Dot)) {
            break;
        }
        i += 2;
    }
    path
}

// the tokens of the statement that `before` ends in
fn statement(before: &str) -> Vec<Token> {
    let mut toks = lex(before).tokens;
    toks.retain(|t| t.kind != Tok::Eof);
    let from = toks
        .iter()
        .rposition(|t| {
            matches!(
                t.kind,
                Tok::Kw(Kw::And | Kw::Then | Kw::ThenIf) | Tok::Semi | Tok::LBrace | Tok::Newline
            )
        })
        .map_or(0, |i| i + 1);
    toks.split_off(from)
}

enum Tail {
    /// The cursor is in a comment.
    Comment,
    /// Inside a string; the byte offset of its opening quote.
    Quoted(usize),
    Plain,
}

fn scan_tail(text: &str) -> Tail {
    let mut open = None;
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        match (open, c) {
            (Some(_), '\\') => {
                chars.next();
            }
            (Some(_), '"') => open = None,
            (None, '"') => open = Some(i),
            (None, '#') => return Tail::Comment,
            _ => {}
        }
    }
    open.map_or(Tail::Plain, Tail::Quoted)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/' | '$')
}

// where the word that ends `text` starts
fn word_start(text: &str) -> usize {
    text.char_indices()
        .rev()
        .take_while(|(_, c)| is_word_char(*c))
        .last()
        .map_or(text.len(), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::commands::lang::parser::tests::{PIECES, STATE};
    use crate::audio_processing::commands::testing::*;
    use std::path::PathBuf;

    /// A temp directory removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("blast-ins-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn write(&self, rel: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session() -> CmdProcessor {
        let mut cp = booted(&["kick", "hat", "snare"]);
        for line in [
            "clock slow 90b",
            "voice kick from \"kick\"",
            "voice hat from \"hat\"",
            "voice snare from \"snare\"",
            "kick { seq beat 1 2 3 4 in 4 | trig }",
            "group drums { hat }",
            "seq top 1 2 in 4 | trig snare",
        ] {
            ok(&mut cp, line);
        }
        cp
    }

    fn comp(cp: &CmdProcessor, line: &str) -> Vec<String> {
        cp.completions_with_home(line, line.len(), None)
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn has(v: &[String], words: &[&str]) {
        for w in words {
            assert!(v.contains(&w.to_string()), "{w} in {v:?}");
        }
    }

    #[test]
    fn complete_statement_start() {
        let cp = session();
        let all = comp(&cp, "");
        has(
            &all,
            &[
                "voice", "clock", "group", "seq", "packet", "track", "start", "remove", "rescan",
                "let", "source", "state", "check", "help", "clear", "quit", "exit", "kick", "main",
                "slow", "drums", "top", "hat",
            ],
        );
        assert!(all.contains(&"wave".to_string()));
        assert!(all.contains(&"lfo".to_string()));
        // after `and`, `then`, `;` and `{` too
        assert_eq!(comp(&cp, "start kick and sta"), strs(&["start", "state"]));
        assert_eq!(comp(&cp, "group g { ki"), strs(&["kick"]));
        assert_eq!(comp(&cp, "start kick; sl"), strs(&["slow"]));
        // after a kind word comes a new name
        assert!(comp(&cp, "voice ").is_empty());
        assert!(comp(&cp, "clock ").is_empty());
    }

    #[test]
    fn complete_settings_of_kind() {
        let cp = session();
        let v = comp(&cp, "kick ");
        has(&v, &["gain=", "g=", "loop=", "l=", "on", "in", "from", "{"]);
        assert!(!v.contains(&"|".to_string()));
        // a setting already given is not offered again, in either form
        let v = comp(&cp, "kick gain=1 ");
        assert!(!v.contains(&"gain=".to_string()) && !v.contains(&"g=".to_string()));
        has(&v, &["pan="]);
        assert_eq!(
            comp(&cp, "kick lo"),
            strs(&["loop=", "loopend=", "loopstart="])
        );
        // a new voice takes the kind's settings
        has(&comp(&cp, "voice a "), &["rate="]);
        // seqs take a pipe and settings; groups a block
        has(&comp(&cp, "kick.beat "), &["|", "jitter="]);
        has(&comp(&cp, "drums "), &["{"]);
        // a clock takes nothing; an unknown name nothing
        assert!(comp(&cp, "clock x 120b ").is_empty());
        assert!(comp(&cp, "nobody ").is_empty());
        // the schema is the source: every built voice setting is offered
        let v = comp(&cp, "kick ");
        for s in built_settings(schema(Kind::Voice)) {
            has(&v, &[&format!("{}=", s.name), &format!("{}=", s.short)]);
        }
    }

    #[test]
    fn complete_word_values() {
        let cp = session();
        assert_eq!(
            comp(&cp, "kick loop="),
            strs(&["forward", "off", "pingpong"])
        );
        assert_eq!(comp(&cp, "kick l=pi"), strs(&["pingpong"]));
        assert_eq!(
            comp(&cp, "voice a from \"kick\" loop=f"),
            strs(&["forward"])
        );
        // numbers have no words
        assert!(comp(&cp, "kick gain=").is_empty());
    }

    #[test]
    fn complete_on() {
        let cp = session();
        assert_eq!(
            comp(&cp, "hat on "),
            strs(&["drums", "hat", "kick", "main", "slow", "snare"])
        );
        assert_eq!(comp(&cp, "hat on sl"), strs(&["slow"]));
        assert_eq!(comp(&cp, "voice a from \"hat\" on m"), strs(&["main"]));
    }

    #[test]
    fn complete_from_tracks() {
        let cp = session();
        assert_eq!(
            comp(&cp, "voice a from "),
            strs(&["\"hat\"", "\"kick\"", "\"snare\""])
        );
        // once the quote is open, the word keeps it
        assert_eq!(comp(&cp, "voice a from \""), comp(&cp, "voice a from "));
        assert_eq!(comp(&cp, "voice a from \"sn"), strs(&["\"snare\""]));
        assert!(comp(&cp, "voice a from \"x").is_empty());
    }

    #[test]
    fn complete_action_targets() {
        let cp = session();
        has(
            &comp(&cp, "start "),
            &["all", "kick", "drums", "main", "slow", "top"],
        );
        assert_eq!(comp(&cp, "stop dr"), strs(&["drums"]));
        // after a comma, more targets but not `all`
        let v = comp(&cp, "pause kick, ");
        has(&v, &["hat"]);
        assert!(!v.contains(&"all".to_string()));
        // remove also takes tracks, quoted
        has(&comp(&cp, "remove "), &["\"kick\""]);
        assert_eq!(comp(&cp, "remove \"ha"), strs(&["\"hat\""]));
        // ungroup takes groups and voices; rescan nothing
        let v = comp(&cp, "ungroup ");
        has(&v, &["drums"]);
        assert!(!v.contains(&"main".to_string()));
        assert!(comp(&cp, "rescan ").is_empty());
        // after a target, nothing
        assert!(comp(&cp, "start kick ").is_empty());
    }

    #[test]
    fn complete_effects() {
        let mut cp = session();
        ok(&mut cp, "kick { filter lo type=hp }");
        // an effect is a name under its voice, and takes its kind's settings
        has(&comp(&cp, "kick.l"), &["lo"]);
        has(
            &comp(&cp, "kick.lo "),
            &["cutoff=", "type=", "base=", "gain="],
        );
        has(&comp(&cp, "kick { drive d "), &["amount=", "mix="]);
        has(&comp(&cp, "kick { crush c "), &["bits=", "down="]);
        has(&comp(&cp, "stop kick.l"), &["lo"]);
    }

    #[test]
    fn complete_pipe() {
        let cp = session();
        assert_eq!(comp(&cp, "kick.beat | "), strs(&["melo", "trig"]));
        assert_eq!(comp(&cp, "kick.beat | trig, "), strs(&["melo", "trig"]));
        assert_eq!(comp(&cp, "kick.beat | tr"), strs(&["trig"]));
        assert_eq!(
            comp(&cp, "kick.beat | trig "),
            strs(&["drums", "hat", "kick", "snare"])
        );
        assert_eq!(comp(&cp, "kick.beat | trig, melo ha"), strs(&["hat"]));
        assert_eq!(comp(&cp, "seq s 1 | "), strs(&["melo", "trig"]));
        // a pipe is offered after a seq's name
        has(&comp(&cp, "seq s 1 2 "), &["|"]);
    }

    #[test]
    fn complete_source_names() {
        let dir = TempDir::new("source");
        dir.write("alpha.blast");
        dir.write("alps.blast");
        dir.write("lib/set-1.blast");
        dir.write("lib/other.blast");
        dir.write(".hidden.blast");
        let mut cp = session();
        cp.set_sources_dir(dir.0.clone());
        assert_eq!(comp(&cp, "source "), strs(&["alpha", "alps", "lib/"]));
        assert_eq!(comp(&cp, "source alp"), strs(&["alpha", "alps"]));
        assert_eq!(comp(&cp, "check al"), strs(&["alpha", "alps"]));
        assert_eq!(comp(&cp, "source lib/"), strs(&["lib/other", "lib/set-1"]));
        assert_eq!(comp(&cp, "source lib/s"), strs(&["lib/set-1"]));
        assert_eq!(comp(&cp, "state to al"), strs(&["alpha", "alps"]));
        assert_eq!(comp(&cp, "state kick.beat to l"), strs(&["lib/"]));
        assert_eq!(comp(&cp, "source alpha "), strs(&["history", "read"]));
        assert!(comp(&cp, "source nothere/").is_empty());
        // `state` offers names and `to`
        has(&comp(&cp, "state "), &["kick", "to"]);
        assert_eq!(comp(&cp, "state kick "), strs(&["to"]));
    }

    #[test]
    fn complete_source_offers_history() {
        let dir = TempDir::new("history");
        dir.write("alpha.blast");
        let mut cp = session();
        cp.set_sources_dir(dir.0.clone());
        has(&comp(&cp, "source alpha "), &["read", "history"]);
        assert_eq!(comp(&cp, "source alpha read "), strs(&["history"]));
        assert_eq!(comp(&cp, "source alpha history "), strs(&["read"]));
        // `check` takes neither
        assert!(comp(&cp, "check alpha ").is_empty());
    }

    #[test]
    fn complete_quoted_path() {
        let dir = TempDir::new("quoted");
        dir.write("sources/a.blast");
        dir.write("sources/sub/b.blast");
        dir.write("home/songs/one.blast");
        dir.write("home/songs/two/x.blast");
        let mut cp = session();
        cp.set_sources_dir(dir.0.join("sources"));
        // relative to the sources folder; files keep their extension
        assert_eq!(comp(&cp, "source \""), strs(&["\"a.blast\"", "\"sub/"]));
        assert_eq!(comp(&cp, "source \"su"), strs(&["\"sub/"]));
        assert_eq!(comp(&cp, "source \"sub/"), strs(&["\"sub/b.blast\""]));
        assert_eq!(comp(&cp, "check \"a"), strs(&["\"a.blast\""]));
        assert_eq!(comp(&cp, "state to \"su"), strs(&["\"sub/"]));
        // `~/` is the home folder (given through the seam)
        let home = Some(dir.0.join("home").into_os_string());
        let at = |line: &str| cp.completions_with_home(line, line.len(), home.clone());
        assert_eq!(at("source \"~"), strs(&["\"~/"]));
        assert_eq!(at("source \"~/"), strs(&["\"~/songs/"]));
        assert_eq!(
            at("source \"~/songs/"),
            strs(&["\"~/songs/one.blast\"", "\"~/songs/two/"])
        );
        assert_eq!(at("source \"~/songs/tw"), strs(&["\"~/songs/two/"]));
        // an absolute path
        let home_dir = dir.0.join("home").display().to_string();
        assert_eq!(
            at(&format!("source \"{home_dir}/")),
            strs(&[&format!("\"{home_dir}/songs/")])
        );
        // without a home, `~/` is just a folder under the sources
        assert!(comp(&cp, "source \"~/").is_empty());
        // a quote elsewhere is not a path
        assert!(comp(&cp, "kick gain=\"").is_empty());
    }

    #[test]
    fn complete_help() {
        let cp = session();
        has(
            &comp(&cp, "help "),
            &[
                "voice", "lfo", "start", "rescan", "let", "source", "quit", "exit", "clear",
            ],
        );
        assert_eq!(comp(&cp, "help sta"), strs(&["start", "state"]));
        assert_eq!(comp(&cp, "help gr"), strs(&["group"]));
        assert!(comp(&cp, "help voice ").is_empty());
    }

    #[test]
    fn complete_by() {
        let cp = session();
        assert_eq!(comp(&cp, "start kick at 4 b"), strs(&["by"]));
        assert_eq!(comp(&cp, "start kick after 2 "), strs(&["by"]));
        assert!(comp(&cp, "start kick at ").is_empty());
        assert_eq!(
            comp(&cp, "start kick at 4 by "),
            strs(&["drums", "hat", "kick", "main", "slow", "snare", "top"])
        );
        assert_eq!(comp(&cp, "start kick at 4 by sl"), strs(&["slow"]));
        assert_eq!(comp(&cp, "clock main 130b after 2 by m"), strs(&["main"]));
    }

    #[test]
    fn complete_dot_path() {
        let cp = session();
        assert_eq!(comp(&cp, "kick."), strs(&["beat"]));
        assert_eq!(comp(&cp, "kick.be"), strs(&["beat"]));
        assert_eq!(comp(&cp, "start kick.b"), strs(&["beat"]));
        // a group's members and seqs
        assert_eq!(comp(&cp, "drums."), strs(&["hat"]));
        assert!(comp(&cp, "drums.hat.").is_empty());
        // not an owner, or a number's fraction
        assert!(comp(&cp, "slow.").is_empty());
        assert!(comp(&cp, "nobody.").is_empty());
        assert!(comp(&cp, "seq s 1.").is_empty());
        assert!(comp(&cp, "seq s 1..").is_empty());
    }

    #[test]
    fn partial_word_filters() {
        let cp = session();
        // only candidates starting with the partial word, sorted, once
        assert_eq!(comp(&cp, "sn"), strs(&["snare"]));
        let want: Vec<String> = comp(&cp, "")
            .into_iter()
            .filter(|w| w.starts_with('s'))
            .collect();
        assert_eq!(comp(&cp, "s"), want);
        let v = comp(&cp, "");
        let mut sorted = v.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(v, sorted);
        assert!(comp(&cp, "zzz").is_empty());
        // the cursor decides, not the end of the line
        assert_eq!(
            cp.completions_with_home("start kick", 3, None),
            strs(&["start", "state"])
        );
        // a comment has no completions; a cursor past the end is the end
        assert!(comp(&cp, "# st").is_empty());
        assert_eq!(cp.completions_with_home("sn", 99, None), strs(&["snare"]));
        // a cursor inside a multi-byte character rounds down
        assert_eq!(cp.completions_with_home("\u{e9}", 1, None), comp(&cp, ""));
    }

    #[test]
    fn names_by_kind() {
        let mut cp = session();
        ok(
            &mut cp,
            "kick { packet fills { seq a 1 in 4 } | trig snare }",
        );
        ok(&mut cp, "packet pk { seq a 1 in 4 } | trig snare");
        assert_eq!(cp.names(NameKind::Clock), strs(&["main", "slow"]));
        assert_eq!(cp.names(NameKind::Voice), strs(&["hat", "kick", "snare"]));
        assert_eq!(cp.names(NameKind::Group), strs(&["drums"]));
        assert_eq!(cp.names(NameKind::Track), strs(&["hat", "kick", "snare"]));
        has(&cp.names(NameKind::Seq), &["kick.beat", "top"]);
        has(&cp.names(NameKind::Packet), &["pk", "kick.fills"]);
        let any = cp.names(NameKind::Any);
        has(&any, &["main", "kick", "drums", "top", "pk", "kick.beat"]);
        assert!(!any.contains(&"\"kick\"".to_string()));
        let mut sorted = any.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(any, sorted);
        // removing drops the name
        ok(&mut cp, "remove top");
        assert!(!cp.names(NameKind::Seq).contains(&"top".to_string()));
    }

    #[test]
    fn never_panics_on_garbage() {
        let cp = session();
        let mut texts = vec![PIECES.to_string(), STATE.to_string()];
        texts.push("(".repeat(80));
        texts.push("{".repeat(80));
        for t in &texts {
            let bytes = t.as_bytes();
            for i in 0..=bytes.len() {
                // a cursor anywhere, even inside a character
                let _ = cp.completions_with_home(t, i, None);
                if let Ok(prefix) = std::str::from_utf8(&bytes[..i]) {
                    let _ = comp(&cp, prefix);
                }
                let mut del = bytes.to_vec();
                if i < del.len() {
                    del.remove(i);
                    if let Ok(s) = String::from_utf8(del) {
                        let _ = comp(&cp, &s);
                    }
                }
            }
        }
        for t in [
            "\u{0}",
            "\u{e9}",
            "\"\u{e9}",
            "\\",
            "$",
            "b(",
            "1..",
            "~~",
            "%%",
            "a.b.",
            "a.b.c.d.",
            ".",
            "..",
            "\"",
            "\"\\",
            "source \"~",
            "source \"\u{e9}/",
            "kick |, ,",
            "kick.\u{e9}.",
            "= =",
            "on on",
            "at at",
            "by",
            "state to to",
        ] {
            let _ = comp(&cp, t);
            let _ = cp.completions_with_home(t, 0, None);
        }
    }
}
