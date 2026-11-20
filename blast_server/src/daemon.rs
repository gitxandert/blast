//! Running in the background: the pid file, detaching, `stop` and `status`.
//!
//! The pid file holds the server's pid on its first line and the URLs it
//! listens on after it. The server writes it once it is listening and
//! removes it on the way out, so a `--bg` start can wait for it to appear.
//! Liveness is checked through `/proc`, so a stale file (after a crash, or
//! a pid reused by another program) reads as "not running".

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set in the detached child of `--bg`.
pub const CHILD_ENV: &str = "BLAST_SERVER_DETACHED";

/// The binary's name, as `/proc/<pid>/comm` shows it.
const COMM: &str = "blast-server";

/// How long `--bg` waits for the server to listen (the engine loads every
/// asset first).
const START_WAIT: Duration = Duration::from_secs(90);
const STOP_WAIT: Duration = Duration::from_secs(10);

/// A server the pid file points at.
pub struct Running {
    pub pid: u32,
    pub urls: Vec<String>,
}

/// Whether `pid` is a blast-server process.
pub fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == COMM)
}

fn parse(text: &str) -> Option<Running> {
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let urls = lines
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect();
    Some(Running { pid, urls })
}

/// The server the pid file names, if it is still running.
pub fn running(pid_file: &Path) -> Option<Running> {
    let r = parse(&std::fs::read_to_string(pid_file).ok()?)?;
    alive(r.pid).then_some(r)
}

/// Records this process as the running server.
pub fn write_pid(pid_file: &Path, urls: &[String]) -> Result<(), String> {
    if let Some(dir) = pid_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut text = format!("{}\n", std::process::id());
    for u in urls {
        text.push_str(u);
        text.push('\n');
    }
    let tmp = pid_file.with_extension("pid.tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, pid_file))
        .map_err(|e| format!("{}: {e}", pid_file.display()))
}

/// Removes the pid file if it still names this process.
pub fn remove_pid(pid_file: &Path) {
    let ours = std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|t| parse(&t))
        .is_some_and(|r| r.pid == std::process::id());
    if ours {
        let _ = std::fs::remove_file(pid_file);
    }
}

/// Starts this binary again, detached, with the same arguments, its output
/// appended to `log_file`; returns once it is listening.
pub fn spawn_background(pid_file: &Path, log_file: &Path) -> Result<Running, String> {
    if let Some(dir) = log_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .map_err(|e| format!("could not open log '{}': {e}", log_file.display()))?;
    let start = log.metadata().map(|m| m.len()).unwrap_or(0);
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| format!("could not find own binary: {e}"))?;
    let mut child = Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        // its own process group: Ctrl-C and the terminal closing don't reach it
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not start the server: {e}"))?;
    let pid = child.id();
    let t0 = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "the server exited during startup ({status}); its log says:\n{}",
                log_since(log_file, start)
            ));
        }
        if let Some(r) = running(pid_file)
            && r.pid == pid
        {
            return Ok(r);
        }
        if t0.elapsed() > START_WAIT {
            return Err(format!(
                "the server (pid {pid}) is not listening after {}s; see {}",
                START_WAIT.as_secs(),
                log_file.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What was appended to the log after byte `start`, at most its last 20
/// lines.
fn log_since(log_file: &Path, start: u64) -> String {
    let text = std::fs::read(log_file).unwrap_or_default();
    let text = String::from_utf8_lossy(text.get(start as usize..).unwrap_or_default());
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

/// Stops the server the pid file names: SIGTERM, then waits for it to go.
/// `Ok(false)` when none was running.
pub fn stop(pid_file: &Path) -> Result<bool, String> {
    let Some(r) = running(pid_file) else {
        let _ = std::fs::remove_file(pid_file);
        return Ok(false);
    };
    let ok = Command::new("kill")
        .args(["-TERM", &r.pid.to_string()])
        .status()
        .map_err(|e| format!("could not run kill: {e}"))?
        .success();
    if !ok {
        return Err(format!("kill -TERM {} failed", r.pid));
    }
    let t0 = Instant::now();
    while alive(r.pid) {
        if t0.elapsed() > STOP_WAIT {
            return Err(format!(
                "pid {} is still running after {}s; `kill -KILL {}` ends it",
                r.pid,
                STOP_WAIT.as_secs(),
                r.pid
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(pid_file);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("blast-server-pid-{}", std::process::id()));
        let path = dir.join("server.pid");
        let urls = vec!["http://127.0.0.1:8800/".to_string()];
        write_pid(&path, &urls).unwrap();
        let r = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(r.pid, std::process::id());
        assert_eq!(r.urls, urls);
        // a test binary isn't blast-server, so the file reads as stale
        assert!(running(&path).is_none());
        remove_pid(&path);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse("").is_none());
        assert!(parse("abc\n").is_none());
        assert_eq!(parse("12\n\n").unwrap().urls, Vec::<String>::new());
    }

    #[test]
    fn stale_pid_is_not_alive() {
        assert!(!alive(std::process::id()));
        assert!(!alive(u32::MAX));
    }
}
