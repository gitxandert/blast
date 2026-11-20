// Command history that survives restarts: a file read once at startup and
// appended to on each Enter, all on the REPL thread.
//
// One entry per line in the file. A multi-line entry (an open block) is
// stored on one line with `\n` written as the two chars `\n`, and a
// backslash as `\\`, so it reads back unchanged.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

/// Entries kept in memory and (after a trim) in the file.
pub const HISTORY_CAP: usize = 1000;

/// The remembered lines, newest last, plus the Up/Down browsing position.
#[derive(Debug)]
pub struct History {
    entries: Vec<String>,
    cap: usize,
    path: Option<PathBuf>,
    // the entry shown while browsing (None = not browsing)
    nav: Option<usize>,
    // what was typed before the first Up
    saved: String,
    // a write failed and was reported; stay quiet from now on
    warned: bool,
}

/// `$XDG_STATE_HOME/blast/history` when that is set, non-empty and absolute,
/// else `$HOME/.local/state/blast/history`, else `None`.
pub fn history_path(xdg_state: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let xdg = xdg_state
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty() && p.is_absolute());
    if let Some(dir) = xdg {
        return Some(dir.join("blast").join("history"));
    }
    let home = home.filter(|h| !h.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("blast")
            .join("history"),
    )
}

fn escape(line: &str) -> String {
    line.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

impl History {
    /// Loads the file (missing = empty). The String is a warning to print.
    pub fn open(path: Option<PathBuf>, cap: usize) -> (Self, Option<String>) {
        let mut history = History::in_memory(cap);
        let Some(file) = path else {
            return (history, None);
        };
        if let Some(dir) = file.parent()
            && let Err(e) = fs::create_dir_all(dir)
        {
            let warning = format!("Warn: history not saved ({}): {e}", dir.display());
            return (history, Some(warning));
        }
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                let warning = format!("Warn: history not read ({}): {e}", file.display());
                return (history, Some(warning));
            }
        };
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        let total = lines.len();
        history.entries = lines
            .into_iter()
            .skip(total.saturating_sub(cap))
            .map(unescape)
            .collect();
        history.path = Some(file.clone());
        // the file only grows by appending, so trim it once it is far over
        let mut warning = None;
        if total > 2 * cap {
            let body: String = history.entries.iter().map(|e| escape(e) + "\n").collect();
            if let Err(e) = fs::write(&file, body) {
                warning = Some(format!("Warn: history not trimmed: {e}"));
            }
        }
        (history, warning)
    }

    /// In memory only (tests, and when no path can be found).
    pub fn in_memory(cap: usize) -> Self {
        History {
            entries: Vec::new(),
            cap,
            path: None,
            nav: None,
            saved: String::new(),
            warned: false,
        }
    }

    /// Skips empty lines and a repeat of the newest; appends to the file.
    /// A write error comes back as a warning, the first time only.
    pub fn push(&mut self, line: &str) -> Option<String> {
        self.nav = None;
        if line.trim().is_empty() || self.entries.last().is_some_and(|l| l == line) {
            return None;
        }
        self.entries.push(line.to_string());
        if self.entries.len() > self.cap {
            self.entries.remove(0);
        }
        let path = self.path.as_ref()?;
        let written = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{}", escape(line)));
        let e = written.err()?;
        self.path = None;
        if self.warned {
            return None;
        }
        self.warned = true;
        Some(format!("Warn: history not saved: {e}"))
    }

    /// `push` for each line in order (`source … history`): the last one is
    /// the newest. Comes back with the first warning, if any.
    pub fn push_many(&mut self, lines: &[String]) -> Option<String> {
        let warnings: Vec<String> = lines.iter().filter_map(|l| self.push(l)).collect();
        warnings.into_iter().next()
    }

    /// Up: the next older entry; `typing` is kept on the first step.
    pub fn prev(&mut self, typing: &str) -> Option<&str> {
        let i = match self.nav {
            None if self.entries.is_empty() => return None,
            None => {
                self.saved = typing.to_string();
                self.entries.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.nav = Some(i);
        self.entries.get(i).map(String::as_str)
    }

    #[allow(clippy::should_implement_trait)]
    /// Down: the next newer entry; past the newest, what was typed.
    pub fn next(&mut self) -> Option<&str> {
        let i = self.nav?;
        if i + 1 < self.entries.len() {
            self.nav = Some(i + 1);
            self.entries.get(i + 1).map(String::as_str)
        } else {
            self.nav = None;
            Some(&self.saved)
        }
    }

    /// The newest entry older than `before` containing `query`.
    pub fn search(&self, query: &str, before: usize) -> Option<usize> {
        let end = before.min(self.entries.len());
        self.entries[..end].iter().rposition(|e| e.contains(query))
    }

    pub fn get(&self, i: usize) -> Option<&str> {
        self.entries.get(i).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // a fresh empty folder under the system temp dir, removed on drop
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("blast-history-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("state").join("history")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mem(lines: &[&str]) -> History {
        let mut h = History::in_memory(10);
        for l in lines {
            h.push(l);
        }
        h
    }

    #[test]
    fn path_prefers_xdg_then_home() {
        let os = |s: &'static str| Some(OsStr::new(s));
        assert_eq!(
            history_path(os("/x/state"), os("/home/u")),
            Some(Path::new("/x/state/blast/history").to_path_buf())
        );
        let home = Some(Path::new("/home/u/.local/state/blast/history").to_path_buf());
        assert_eq!(history_path(os("rel/state"), os("/home/u")), home);
        assert_eq!(history_path(os(""), os("/home/u")), home);
        assert_eq!(history_path(None, os("/home/u")), home);
        assert_eq!(history_path(None, None), None);
        assert_eq!(history_path(os("rel"), os("")), None);
    }

    #[test]
    fn push_skips_empty_and_repeats() {
        let h = mem(&["a", "", "   ", "a", "b", "a"]);
        assert_eq!(h.len(), 3);
        assert_eq!(h.get(0), Some("a"));
        assert_eq!(h.get(1), Some("b"));
        assert_eq!(h.get(2), Some("a"));
    }

    #[test]
    fn cap_drops_oldest() {
        let mut h = History::in_memory(3);
        for l in ["1", "2", "3", "4", "5"] {
            h.push(l);
        }
        assert_eq!(h.len(), 3);
        assert_eq!(h.get(0), Some("3"));
        assert_eq!(h.get(2), Some("5"));
    }

    #[test]
    fn reopen_reads_back_and_trims_long_files() {
        let dir = TempDir::new("reopen");
        let (mut h, warn) = History::open(Some(dir.file()), 3);
        assert!(warn.is_none());
        for l in ["a", "b", "c"] {
            assert!(h.push(l).is_none());
        }
        let (h, _) = History::open(Some(dir.file()), 3);
        assert_eq!(h.len(), 3);
        assert_eq!(h.get(0), Some("a"));
        drop(h);

        // past 2 * cap lines the file is rewritten to the last `cap`
        let (mut h, _) = History::open(Some(dir.file()), 3);
        for l in ["d", "e", "f", "g", "h"] {
            h.push(l);
        }
        let (h, _) = History::open(Some(dir.file()), 3);
        assert_eq!(h.len(), 3);
        assert_eq!(h.get(0), Some("f"));
        assert_eq!(h.get(2), Some("h"));
        assert_eq!(fs::read_to_string(dir.file()).unwrap(), "f\ng\nh\n");
    }

    #[test]
    fn prev_next_restore_what_was_typed() {
        let mut h = mem(&["one", "two"]);
        assert_eq!(h.prev("typ"), Some("two"));
        assert_eq!(h.prev("ignored"), Some("one"));
        // the oldest stays put
        assert_eq!(h.prev("ignored"), Some("one"));
        assert_eq!(h.next(), Some("two"));
        assert_eq!(h.next(), Some("typ"));
        assert_eq!(h.next(), None);
        assert_eq!(History::in_memory(3).prev("x"), None);
    }

    #[test]
    fn search_finds_newest_older_match() {
        let h = mem(&["kick start", "snare", "kick stop", "hat"]);
        assert_eq!(h.search("kick", h.len()), Some(2));
        assert_eq!(h.search("kick", 2), Some(0));
        assert_eq!(h.search("kick", 0), None);
        assert_eq!(h.search("zzz", h.len()), None);
        assert_eq!(h.search("", h.len()), Some(3));
    }

    #[test]
    fn push_many_appends_in_order() {
        let dir = TempDir::new("many");
        let (mut h, _) = History::open(Some(dir.file()), 10);
        h.push("first");
        let lines: Vec<String> = ["a", "", "b\nc"].iter().map(|s| s.to_string()).collect();
        assert!(h.push_many(&lines).is_none());
        assert_eq!(h.len(), 3);
        // Up gives the last statement first
        assert_eq!(h.prev(""), Some("b\nc"));
        assert_eq!(h.prev(""), Some("a"));
        let (h, _) = History::open(Some(dir.file()), 10);
        assert_eq!(h.get(2), Some("b\nc"));
    }

    #[test]
    fn multiline_entry_round_trips() {
        let dir = TempDir::new("multi");
        let entry = "kick {\n  lfo w to=pan\\x\n}";
        let (mut h, _) = History::open(Some(dir.file()), 10);
        h.push(entry);
        let raw = fs::read_to_string(dir.file()).unwrap();
        assert_eq!(raw.lines().count(), 1);
        let (h, _) = History::open(Some(dir.file()), 10);
        assert_eq!(h.get(0), Some(entry));
    }

    #[test]
    fn unwritable_path_warns_once() {
        let dir = TempDir::new("unwritable");
        // a file where the folder should be
        let blocker = dir.0.join("blocker");
        fs::write(&blocker, "x").unwrap();
        let (mut h, warn) = History::open(Some(blocker.join("history")), 10);
        assert!(warn.is_some());
        assert!(h.push("a").is_none());

        // the folder opens fine, then the history path becomes a directory
        let (mut h, warn) = History::open(Some(dir.file()), 10);
        assert!(warn.is_none());
        fs::create_dir(dir.file()).unwrap();
        assert!(h.push("a").is_some());
        assert!(h.push("b").is_none());
        assert_eq!(h.len(), 2);
    }
}
