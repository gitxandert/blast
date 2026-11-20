//! Scripts (REQ-056 §5–§7): `--script`, `source` and `check` read a file,
//! check the whole of it first against a copy of the session (every error
//! reported, nothing applied), and only then apply it line by line.

use super::ast::{Line, Script, SourcePath, SourceStmt, Stmt};
use super::errors::{Code, LangErr, Source, Span};
use super::lower::{Errs, Out, Write};
use super::parser::{Parse, parse};
use super::runner::{is_timed, script_name};
use crate::audio_processing::commands::{BatchArgs, CmdErr, CmdProcessor, CmdSender, Command};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The most files that may be open at once: a script and what it sources.
const MAX_DEPTH: usize = 16;

/// How long `run_script` waits for room in the command queue, per line.
const QUEUE_WAIT_MS: u32 = 2000;

fn with_blast(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".blast");
    PathBuf::from(s)
}

pub(super) fn tag(errs: Vec<LangErr>, file: &str) -> Vec<LangErr> {
    errs.into_iter().map(|e| e.in_file(file)).collect()
}

/// A path as shown in errors: relative to the working directory if it is
/// inside it.
pub(super) fn shown(path: &Path) -> String {
    let rel = std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf));
    rel.as_deref().unwrap_or(path).display().to_string()
}

/// The `source` statement a line consists of, if it is nothing else and
/// runs the file (`read` and `history` lower as queries).
pub(super) fn lone_source(line: &Line) -> Option<&SourceStmt> {
    match (line.chain(), &line.when) {
        ([Stmt::Source(s)], None) if !s.read && !s.history && !s.stop => Some(s),
        _ => None,
    }
}

/// The wait of a line that is nothing but `end after D`.
fn lone_end_after(line: &Line) -> Option<&super::ast::Wait> {
    match (line.chain(), &line.when) {
        ([Stmt::End(e)], None) if line.joins.is_empty() => e.after.as_ref(),
        _ => None,
    }
}

/// E202 for a file that ends inside a block or a continued line: at the
/// last `{` that is never closed, else at the last thing written.
fn unclosed(text: &str) -> LangErr {
    let mut open: Vec<(u32, u32)> = Vec::new();
    let (mut line, mut col) = (1u32, 1u32);
    let (mut in_str, mut esc, mut in_comment) = (false, false, false);
    let mut last = (1u32, 1u32);
    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
            in_comment = false;
            continue;
        }
        if in_comment {
            col += 1;
            continue;
        }
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
        } else {
            match ch {
                '#' => in_comment = true,
                '"' => in_str = true,
                '{' => open.push((line, col)),
                '}' => {
                    open.pop();
                }
                _ => {}
            }
        }
        if !in_comment && !ch.is_whitespace() {
            last = (line, col);
        }
        col += 1;
    }
    let (message, (line, col)) = match open.last() {
        Some(&at) => ("this block is never closed", at),
        None => ("the file ends in the middle of a statement", last),
    };
    LangErr::new(Code::UnclosedBlock, Span { line, col, len: 1 }, message)
}

pub(super) fn wait_for_room(queue: &CmdSender, span: Span) -> Result<(), LangErr> {
    for _ in 0..QUEUE_WAIT_MS {
        if !queue.is_full() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if queue.is_full() {
        return Err(LangErr::new(
            Code::QueueFull,
            span,
            "the command queue stayed full for 2 s",
        ));
    }
    Ok(())
}

/// The file's text, or E701.
fn read_source(path: &Path, span: Span) -> Result<String, Errs> {
    std::fs::read_to_string(path).map_err(|e| {
        let why = match e.kind() {
            std::io::ErrorKind::NotFound => "no such file".to_string(),
            _ => e.to_string(),
        };
        Errs(vec![LangErr::new(
            Code::SourceNotFound,
            span,
            format!("can't read '{}': {why}", shown(path)),
        )])
    })
}

/// The source text of each top-level statement of `script`, as written
/// (a block keeps its lines): from where it starts to where the next one
/// starts, minus the `;`, blanks and comment-only lines after it.
fn statement_texts(text: &str, script: &Script) -> Vec<String> {
    let lines: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
    let at = |line: u32, col: u32| (line as usize - 1, col as usize - 1);
    let mut out = Vec::new();
    for (i, l) in script.lines.iter().enumerate() {
        let (sl, sc) = at(l.span.line, l.span.col);
        let (el, ec) = match script.lines.get(i + 1) {
            Some(next) => at(next.span.line, next.span.col),
            None => (lines.len() - 1, lines[lines.len() - 1].len()),
        };
        let mut rows: Vec<String> = Vec::new();
        for (no, chars) in lines.iter().enumerate().take(el + 1).skip(sl) {
            let from = if no == sl { sc } else { 0 };
            let to = if no == el { ec } else { chars.len() };
            rows.push(chars[from.min(to)..to].iter().collect());
        }
        // drop what follows the statement's last token
        while let Some(last) = rows.last_mut() {
            let kept = last.trim_end().trim_end_matches(';').trim_end().len();
            last.truncate(kept);
            let t = last.trim_start();
            let blank = t.is_empty() || t.starts_with('#');
            if rows.len() > 1 && blank {
                rows.pop();
            } else {
                break;
            }
        }
        let text = rows.join("\n").trim().to_string();
        if !text.is_empty() {
            out.push(text);
        }
    }
    out
}

impl CmdProcessor {
    /// A copy to check a script against: same mirror and variables, nothing
    /// pending.
    fn fork(&self) -> CmdProcessor {
        CmdProcessor {
            engine_state: self.engine_state.clone(),
            pending: String::new(),
            vars: self.vars.clone(),
            src_stack: self.src_stack.clone(),
            outbox: String::new(),
            clear_requested: false,
            history_out: Vec::new(),
            sources_dir: self.sources_dir.clone(),
            checking: true,
            runner: self.runner.fork(),
        }
    }

    /// Checks the whole script against a copy of the session, and on
    /// success applies it line by line, each line one batch. If the check
    /// finds errors, every one is reported and nothing is applied.
    pub fn run_script(&mut self, path: &Path, queue: &mut CmdSender) -> Result<(), CmdErr> {
        self.run_script_text(path, queue).map(|_| ())
    }

    /// Like `run_script`, and answers what the script's queries (`state`,
    /// `help`) printed.
    pub fn run_script_text(
        &mut self,
        path: &Path,
        queue: &mut CmdSender,
    ) -> Result<String, CmdErr> {
        self.check_script(path)?;
        let name = shown(path);
        if let Some(flat) = self.flatten_file(path, Span::default())
            && is_timed(&flat)
        {
            return self.run_timed(&script_name(path), flat, queue);
        }
        let mut texts = Vec::new();
        let done = self.with_file(path, Span::default(), |me, script, name| {
            me.apply_lines(script, name, queue, &mut texts)
        });
        match done {
            Err(errs) => Err(CmdErr::Lang(tag(errs, &name))),
            Ok(Err(errs)) => Err(CmdErr::Lang(errs)),
            Ok(Ok(())) => Ok(texts.join("\n")),
        }
    }

    /// `run_script_text` for text that isn't a file (`name` labels errors).
    /// Answers what the queries printed, and the warnings of the check.
    pub fn run_text(
        &mut self,
        name: &str,
        text: &str,
        queue: &mut CmdSender,
    ) -> Result<(String, Vec<LangErr>), CmdErr> {
        let warns = self.check_text(name, text)?;
        if let Some(flat) = self.flatten_text(name, text)
            && is_timed(&flat)
        {
            let texts = self.run_timed(name, flat, queue)?;
            return Ok((texts, warns));
        }
        let mut texts = Vec::new();
        let done = self.with_text(name, text, |me, script, name| {
            me.apply_lines(script, name, queue, &mut texts)
        });
        match done {
            Err(errs) => Err(CmdErr::Lang(tag(errs, name))),
            Ok(Err(errs)) => Err(CmdErr::Lang(errs)),
            Ok(Ok(())) => Ok((texts.join("\n"), warns)),
        }
    }

    /// `check_script` for text.
    pub fn check_text(&self, name: &str, text: &str) -> Result<Vec<LangErr>, CmdErr> {
        let mut me = self.fork();
        let (mut errs, mut warns) = (Vec::new(), Vec::new());
        let done = me.with_text(name, text, |me, script, name| {
            me.check_lines(script, name, &mut errs, &mut warns)
        });
        if let Err(e) = done {
            errs.extend(tag(e, name));
        }
        if errs.is_empty() {
            Ok(warns)
        } else {
            Err(CmdErr::Lang(errs))
        }
    }

    /// Checks the script without changing the session: every error is
    /// reported (`CmdErr::Lang`), or the warnings come back.
    pub fn check_script(&self, path: &Path) -> Result<Vec<LangErr>, CmdErr> {
        self.check_script_at(path, Span::default())
    }

    fn check_script_at(&self, path: &Path, span: Span) -> Result<Vec<LangErr>, CmdErr> {
        let mut me = self.fork();
        let name = shown(path);
        let (mut errs, mut warns) = (Vec::new(), Vec::new());
        let done = me.with_file(path, span, |me, script, name| {
            me.check_lines(script, name, &mut errs, &mut warns)
        });
        if let Err(e) = done {
            errs.extend(tag(e, &name));
        }
        if errs.is_empty() {
            Ok(warns)
        } else {
            Err(CmdErr::Lang(errs))
        }
    }

    /// `check FILE` in a statement.
    pub(super) fn check_written(&self, written: &SourcePath) -> Result<Vec<LangErr>, Errs> {
        let path = self.resolve_source(written);
        self.check_script_at(&path, written.span())
            .map_err(|e| match e {
                CmdErr::Lang(errs) => Errs(errs),
                other => Errs(vec![LangErr::new(
                    Code::WrongType,
                    written.span(),
                    other.to_string(),
                )]),
            })
    }

    /// `source "file"` inside a statement: its lines join the statement's
    /// commands.
    pub(super) fn lower_source(&mut self, s: &SourceStmt, out: &mut Out) -> Result<(), Errs> {
        if s.stop {
            return self.source_stop(s, out);
        }
        let path = self.resolve_source(&s.path);
        if s.history {
            return self.source_history(s, &path, out);
        }
        if s.read {
            let text = read_source(&path, s.span)?;
            out.texts.push(text.trim_end_matches('\n').to_string());
            return Ok(());
        }
        // a script with waits or an `end` is checked as a whole, then runs
        // in the background
        if let Some(flat) = self.flatten_file(&path, s.span)
            && is_timed(&flat)
        {
            let warns = self.check_written(&s.path)?;
            out.warns.extend(warns);
            return self.start_timed(&script_name(&path), flat, out);
        }
        let done = self.with_file(&path, s.span, |me, script, name| {
            for line in &script.lines {
                me.lower_line(line, out)
                    .map_err(|Errs(errs)| Errs(tag(errs, name)))?;
            }
            Ok(())
        });
        match done {
            Err(errs) => Err(Errs(errs)),
            Ok(done) => done,
        }
    }

    /// `source NAME history`: the file's top-level statements go to
    /// `history_out`, in file order. Nothing is run or checked, but the
    /// file must parse so its statements can be told apart.
    fn source_history(&mut self, s: &SourceStmt, path: &Path, out: &mut Out) -> Result<(), Errs> {
        let text = read_source(path, s.span)?;
        let name = shown(path);
        let script = match parse(Source {
            name: &name,
            text: &text,
        }) {
            Err(errs) => return Err(Errs(tag(errs, &name))),
            Ok(Parse::Incomplete) => return Err(Errs(vec![unclosed(&text).in_file(&name)])),
            Ok(Parse::Complete(script)) => script,
        };
        let statements = statement_texts(&text, &script);
        if s.read {
            out.texts.push(text.trim_end_matches('\n').to_string());
        }
        let n = statements.len();
        let file = path
            .file_name()
            .map_or(name, |f| f.to_string_lossy().into());
        out.texts.push(format!(
            "history: {n} statement{} from {file}",
            if n == 1 { "" } else { "s" }
        ));
        if !self.checking {
            self.history_out.extend(statements);
        }
        Ok(())
    }

    /// A source file as written. A bare name is looked up in the sources
    /// folder: `NAME.blast` first if it has no extension, then as written.
    /// A quoted path is taken as written, except that `~/` is the home
    /// folder and a relative path starts at the sources folder.
    pub fn resolve_source(&self, written: &SourcePath) -> PathBuf {
        let path = self.join_source(written);
        match written {
            SourcePath::Name(..) if path.extension().is_none() => {
                let with = with_blast(&path);
                if with.exists() { with } else { path }
            }
            _ => path,
        }
    }

    fn join_source(&self, written: &SourcePath) -> PathBuf {
        self.join_source_with(written, std::env::var_os("HOME"))
    }

    pub(crate) fn join_source_with(
        &self,
        written: &SourcePath,
        home: Option<std::ffi::OsString>,
    ) -> PathBuf {
        match written {
            SourcePath::Name(n, _) => self.sources_dir.join(n),
            SourcePath::Quoted(q, _) => {
                if let Some(rest) = q.strip_prefix("~/")
                    && let Some(home) = home
                {
                    return PathBuf::from(home).join(rest);
                }
                self.sources_dir.join(q) // an absolute path replaces the folder
            }
        }
    }

    /// Where `state … to NAME` writes: as `resolve_source`, with `.blast`
    /// added when there's no extension.
    pub(super) fn write_target(&self, written: &SourcePath) -> PathBuf {
        let path = self.join_source(written);
        if path.extension().is_none() {
            with_blast(&path)
        } else {
            path
        }
    }

    /// `--script NAME`: an existing file relative to the working directory
    /// wins (shell completion), else it is a source name.
    pub fn resolve_script_arg(&self, arg: &Path) -> PathBuf {
        if arg.is_file() {
            return arg.to_path_buf();
        }
        self.resolve_source(&SourcePath::Name(
            arg.display().to_string(),
            Span::default(),
        ))
    }

    /// Writes the files a line asked for (never in a check). Returns what to
    /// answer, and the errors of the writes that failed.
    pub(super) fn perform_writes(&self, writes: Vec<Write>) -> (Vec<String>, Vec<LangErr>) {
        let (mut said, mut errs) = (Vec::new(), Vec::new());
        if self.checking {
            return (said, errs);
        }
        for w in writes {
            let replaced = w.path.exists();
            let shown = self.shown_in_sources(&w.path);
            let done = match w.path.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => std::fs::create_dir_all(dir),
                _ => Ok(()),
            }
            .and_then(|()| std::fs::write(&w.path, &w.text));
            match done {
                Ok(()) => said.push(format!(
                    "wrote {shown}{}",
                    if replaced { " (replaced)" } else { "" }
                )),
                Err(e) => errs.push(LangErr::new(
                    Code::SourceWrite,
                    w.span,
                    format!("can't write '{shown}': {e}"),
                )),
            }
        }
        (said, errs)
    }

    /// `path` relative to the sources folder's parent, if inside it.
    fn shown_in_sources(&self, path: &Path) -> String {
        match self
            .sources_dir
            .parent()
            .and_then(|p| path.strip_prefix(p).ok())
        {
            Some(rel) => rel.display().to_string(),
            None => path.display().to_string(),
        }
    }

    /// Reads and parses `path`, refusing a cycle (E702), too deep a chain
    /// (E703) and a file that can't be read (E701), then runs `f` on it with
    /// the file on the stack. Parse errors come back tagged with the file.
    pub(super) fn with_file<R>(
        &mut self,
        path: &Path,
        span: Span,
        f: impl FnOnce(&mut Self, &Script, &str) -> R,
    ) -> Result<R, Vec<LangErr>> {
        let name = shown(path);
        let unreadable = |why: String| {
            vec![LangErr::new(
                Code::SourceNotFound,
                span,
                format!("can't read '{name}': {why}"),
            )]
        };
        let canon = std::fs::canonicalize(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => unreadable("no such file".into()),
            _ => unreadable(e.to_string()),
        })?;
        self.guard_source(&canon, &name, span)?;
        let text = std::fs::read_to_string(&canon).map_err(|e| unreadable(e.to_string()))?;
        self.with_parsed(canon, &name, &text, f)
    }

    /// `with_file` for text that isn't a file (`name` labels it).
    pub(super) fn with_text<R>(
        &mut self,
        name: &str,
        text: &str,
        f: impl FnOnce(&mut Self, &Script, &str) -> R,
    ) -> Result<R, Vec<LangErr>> {
        let id = PathBuf::from(format!("<{name}>"));
        self.guard_source(&id, name, Span::default())?;
        self.with_parsed(id, name, text, f)
    }

    // E702 for a source already on the stack, E703 for too deep a chain
    fn guard_source(&self, canon: &Path, name: &str, span: Span) -> Result<(), Vec<LangErr>> {
        if self.src_stack.iter().any(|p| p == canon) {
            return Err(vec![LangErr::new(
                Code::SourceCycle,
                span,
                format!("'{name}' is already being sourced (a cycle)"),
            )]);
        }
        if self.src_stack.len() >= MAX_DEPTH {
            return Err(vec![LangErr::new(
                Code::SourceDepth,
                span,
                format!("files source at most {MAX_DEPTH} deep"),
            )]);
        }
        Ok(())
    }

    // parses `text`, then runs `f` with `canon` on the stack
    fn with_parsed<R>(
        &mut self,
        canon: PathBuf,
        name: &str,
        text: &str,
        f: impl FnOnce(&mut Self, &Script, &str) -> R,
    ) -> Result<R, Vec<LangErr>> {
        let script = match parse(Source { name, text }) {
            Err(errs) => return Err(tag(errs, name)),
            Ok(Parse::Incomplete) => return Err(vec![unclosed(text).in_file(name)]),
            Ok(Parse::Complete(script)) => script,
        };
        self.src_stack.push(canon);
        let result = f(self, &script, name);
        self.src_stack.pop();
        Ok(result)
    }

    /// Lowers every line against the session, going on after a line that
    /// fails (the session is put back to what it was before that line), so
    /// that every error is found.
    fn check_lines(
        &mut self,
        script: &Script,
        file: &str,
        errs: &mut Vec<LangErr>,
        warns: &mut Vec<LangErr>,
    ) {
        for line in &script.lines {
            if let Some(src) = lone_source(line) {
                let path = self.resolve_source(&src.path);
                let done = self.with_file(&path, src.span, |me, sub, name| {
                    me.check_lines(sub, name, errs, warns)
                });
                if let Err(e) = done {
                    errs.extend(tag(e, file));
                }
                continue;
            }
            // a lone `wait` only has to be good; so has the wait of `end after`
            if let Some(w) = line.lone_wait() {
                self.check_wait(w, file, errs);
                continue;
            }
            if let Some(w) = lone_end_after(line) {
                self.check_wait(w, file, errs);
                continue;
            }
            let snapshot = (self.engine_state.clone(), self.vars.clone());
            let mut out = Out::default();
            // after a wait the line waits too (PKb2)
            self.engine_state.staging = self.waited_in_check();
            let lowered = self
                .lower_line(line, &mut out)
                .and_then(|()| self.check_after_wait(line, &out));
            self.engine_state.staging = false;
            match lowered {
                Ok(()) => warns.extend(tag(out.warns, file)),
                Err(Errs(es)) => {
                    errs.extend(tag(es, file));
                    (self.engine_state, self.vars) = snapshot;
                }
            }
        }
    }

    /// Runs a timed script (`start_timed`): what its first segments need
    /// goes to the queue as one command. Answers what its queries printed.
    fn run_timed(
        &mut self,
        name: &str,
        flat: Vec<super::runner::FlatLine>,
        queue: &mut CmdSender,
    ) -> Result<String, CmdErr> {
        wait_for_room(queue, Span::default()).map_err(|e| CmdErr::Lang(vec![e]))?;
        let snapshot = (self.engine_state.clone(), self.vars.clone());
        let runner_before = self.runner.clone();
        let mut out = Out::default();
        if let Err(Errs(es)) = self.start_timed(name, flat, &mut out) {
            (self.engine_state, self.vars) = snapshot;
            self.runner = runner_before;
            return Err(CmdErr::Lang(es));
        }
        let mut texts = std::mem::take(&mut out.texts);
        texts.extend(out.warns.iter().map(ToString::to_string));
        let (wrote, failed) = self.perform_writes(out.writes);
        texts.extend(wrote);
        texts.extend(failed.iter().map(ToString::to_string));
        let cmd = match out.cmds.len() {
            0 => return Ok(texts.join("\n")),
            1 => out.cmds.remove(0),
            _ => Command::Batch(BatchArgs { cmds: out.cmds }),
        };
        // can't fail: there was room, and this thread is the only producer
        queue.try_push(cmd).map_err(|_| {
            CmdErr::Lang(vec![LangErr::new(
                Code::QueueFull,
                Span::default(),
                "command queue full",
            )])
        })?;
        Ok(texts.join("\n"))
    }

    /// Applies a checked script: one batch per line.
    fn apply_lines(
        &mut self,
        script: &Script,
        file: &str,
        queue: &mut CmdSender,
        texts: &mut Vec<String>,
    ) -> Result<(), Vec<LangErr>> {
        for line in &script.lines {
            if let Some(src) = lone_source(line) {
                let path = self.resolve_source(&src.path);
                let done = self.with_file(&path, src.span, |me, sub, name| {
                    me.apply_lines(sub, name, queue, texts)
                });
                match done {
                    Err(e) => return Err(tag(e, file)),
                    Ok(inner) => inner?,
                }
                continue;
            }
            // wait before lowering, so a timeout leaves the session as it is
            wait_for_room(queue, line.span).map_err(|e| vec![e.in_file(file)])?;
            let snapshot = (self.engine_state.clone(), self.vars.clone());
            let mut out = Out::default();
            if let Err(Errs(es)) = self.lower_line(line, &mut out) {
                (self.engine_state, self.vars) = snapshot;
                return Err(tag(es, file));
            }
            self.send_shifters(&mut out.cmds);
            texts.extend(out.texts);
            // what the line removed is worth hearing about (W101 is not:
            // a sourced script restates things freely)
            texts.extend(
                out.warns
                    .iter()
                    .filter(|w| matches!(w.code, Code::ReceiverDropped | Code::WaitingLineDropped))
                    .map(|w| w.clone().in_file(file).to_string()),
            );
            let (wrote, failed) = self.perform_writes(out.writes);
            texts.extend(wrote);
            let cmd = match out.cmds.len() {
                0 if failed.is_empty() => continue,
                0 => return Err(tag(failed, file)),
                1 => out.cmds.remove(0),
                _ => Command::Batch(BatchArgs { cmds: out.cmds }),
            };
            // can't fail: there was room, and this thread is the only
            // producer
            if queue.try_push(cmd).is_err() {
                return Err(vec![
                    LangErr::new(Code::QueueFull, line.span, "command queue full").in_file(file),
                ]);
            }
            if !failed.is_empty() {
                return Err(tag(failed, file));
            }
        }
        Ok(())
    }
}
