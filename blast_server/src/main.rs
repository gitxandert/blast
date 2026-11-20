//! blast server: the BLAST REPL in a browser, with the engine's output
//! streamed to the browser and the output device switchable from it.
//! `blast server ARGS` runs this binary.

mod config;
mod daemon;
mod engine;
mod server;

use axum::serve::ListenerExt;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use lexopt::prelude::*;

const USAGE: &str = "\
Usage: blast server [COMMAND] [OPTIONS]

Serves the BLAST web UI: the REPL in a browser, with the engine's output
streamed to it.

Commands:
  start           run the server (the default)
  stop            stop the running server
  status          say whether a server is running, and where
  config          print the config file's path

Options (each overrides the config file):
  --config <PATH>    config file (default: ~/.config/blast/server.toml,
                     written with every default when missing)
  --port <N>         port to listen on [server.port]
  --host <ADDR>      address to listen on [server.host]
  --bg               run in the background, logging to the log file
  --log <FILE>       the background server's log [server.log_file]
  --device <ID>      output device; \"none\" plays to browsers only
                     [engine.device]
  --assets <DIR>     audio files to load [engine.assets_dir]
  --sources <DIR>    source files folder [engine.sources_dir]
  --script <NAME>    script to run at startup [engine.script]
  --web-root <DIR>   serve the app from DIR instead of the built-in copy
                     [server.web_root]
  --list-devices     print the output devices and exit
  -h, --help         print this help";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Start,
    Stop,
    Status,
    Config,
}

#[derive(Debug, Default)]
struct Args {
    cmd: Option<Cmd>,
    config: Option<PathBuf>,
    port: Option<u16>,
    host: Option<String>,
    background: bool,
    log: Option<PathBuf>,
    device: Option<String>,
    assets: Option<PathBuf>,
    sources: Option<PathBuf>,
    script: Option<String>,
    web_root: Option<PathBuf>,
    list_devices: bool,
    help: bool,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Args, lexopt::Error> {
    let mut a = Args::default();
    let mut p = lexopt::Parser::from_args(args);
    while let Some(arg) = p.next()? {
        match arg {
            Long("config") => a.config = Some(p.value()?.into()),
            Long("port") => a.port = Some(p.value()?.parse()?),
            Long("host") => a.host = Some(p.value()?.string()?),
            Long("bg") => a.background = true,
            Long("log") => a.log = Some(p.value()?.into()),
            Long("device") => a.device = Some(p.value()?.string()?),
            Long("assets") => a.assets = Some(p.value()?.into()),
            Long("sources") => a.sources = Some(p.value()?.into()),
            Long("script") => a.script = Some(p.value()?.string()?),
            Long("web-root") => a.web_root = Some(p.value()?.into()),
            Long("list-devices") => a.list_devices = true,
            Short('h') | Long("help") => a.help = true,
            Value(v) if a.cmd.is_none() => {
                a.cmd = Some(match v.to_str() {
                    Some("start") => Cmd::Start,
                    Some("stop") => Cmd::Stop,
                    Some("status") => Cmd::Status,
                    Some("config") => Cmd::Config,
                    _ => return Err(Value(v).unexpected()),
                })
            }
            other => return Err(other.unexpected()),
        }
    }
    Ok(a)
}

/// A path from the command line: relative to the working directory.
fn from_cwd(p: PathBuf) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    config::resolve(&cwd, &p)
}

/// The config file (written with the defaults if missing), then the flags.
fn load_config(args: &Args) -> Result<config::Config, String> {
    let path = match &args.config {
        Some(p) => from_cwd(p.clone()),
        None => config::default_path()
            .ok_or("no HOME or XDG_CONFIG_HOME to find the config in; pass --config")?,
    };
    if config::ensure_file(&path)? {
        eprintln!(
            "blast server: wrote the default config to {}",
            path.display()
        );
        old_config_hint();
    }
    let mut cfg = config::Config::load(Some(&path))?;
    let s = &mut cfg.server;
    if let Some(p) = args.port {
        s.port = p;
    }
    if let Some(h) = &args.host {
        s.host = h.clone();
    }
    if let Some(l) = &args.log {
        s.log_file = from_cwd(l.clone());
    }
    if let Some(w) = &args.web_root {
        s.web_root = from_cwd(w.clone());
    }
    let e = &mut cfg.engine;
    if let Some(d) = &args.device {
        e.device = d.clone();
    }
    if let Some(d) = &args.assets {
        e.assets_dir = from_cwd(d.clone());
    }
    if let Some(d) = &args.sources {
        e.sources_dir = from_cwd(d.clone());
    }
    if let Some(s) = &args.script {
        e.script = s.clone();
    }
    cfg.validate()?;
    Ok(cfg)
}

/// The prototype kept its config elsewhere; say so once.
fn old_config_hint() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let old = Path::new(&home).join(".config/blast-web/config.toml");
    if old.exists() {
        eprintln!(
            "blast server: blast-web's config is at {}; copy what you need \
             (its `bind` is now `host` and `port`)",
            old.display()
        );
    }
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.list_devices {
        for d in engine::output::list_devices(48_000) {
            println!(
                "{} {}  {}  ({})",
                if d.is_default { '*' } else { ' ' },
                d.id,
                d.name,
                d.summary
            );
        }
        return ExitCode::SUCCESS;
    }
    let cfg = match load_config(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let pid_file = cfg.server.pid_file.clone();
    match args.cmd.unwrap_or(Cmd::Start) {
        Cmd::Config => {
            println!("{}", cfg.path.as_deref().unwrap_or(Path::new("")).display());
            ExitCode::SUCCESS
        }
        Cmd::Status => match daemon::running(&pid_file) {
            Some(r) => {
                println!("blast server is running (pid {})", r.pid);
                for u in r.urls {
                    println!("  {u}");
                }
                println!("  log: {}", cfg.server.log_file.display());
                ExitCode::SUCCESS
            }
            None => {
                println!("blast server is not running");
                // LSB: 3 = not running
                ExitCode::from(3)
            }
        },
        Cmd::Stop => match daemon::stop(&pid_file) {
            Ok(true) => {
                println!("blast server stopped");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!("blast server is not running");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("Error: {e}");
                ExitCode::FAILURE
            }
        },
        Cmd::Start => start(cfg, args.background),
    }
}

fn start(cfg: config::Config, background: bool) -> ExitCode {
    let detached = std::env::var_os(daemon::CHILD_ENV).is_some();
    if !detached && let Some(r) = daemon::running(&cfg.server.pid_file) {
        eprintln!(
            "Error: blast server is already running (pid {}){}; `blast server stop` stops it",
            r.pid,
            r.urls
                .first()
                .map(|u| format!(" at {u}"))
                .unwrap_or_default()
        );
        return ExitCode::FAILURE;
    }
    if background && !detached {
        return match daemon::spawn_background(&cfg.server.pid_file, &cfg.server.log_file) {
            Ok(r) => {
                println!("blast server running in the background (pid {})", r.pid);
                for u in &r.urls {
                    println!("  {u}");
                }
                println!("  log: {}", cfg.server.log_file.display());
                println!("  stop it with `blast server stop`");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("Error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    serve(Arc::new(cfg), detached)
}

/// Runs the server in this process until SIGINT or SIGTERM.
fn serve(cfg: Arc<config::Config>, detached: bool) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(p) = &cfg.path {
        eprintln!("blast server: config {}", p.display());
    }
    // listen first, so a port in use fails before the assets load
    let listeners = match rt.block_on(bind_all(&cfg)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let urls: Vec<String> = listeners
        .iter()
        .filter_map(|l| l.local_addr().ok())
        .map(|a| format!("http://{a}/"))
        .collect();

    let engine = match engine::Engine::start(cfg.clone()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let info = engine.info();
    eprintln!(
        "blast server: engine at {} Hz on {}; assets {}; sources {}",
        info.rate, info.output.name, info.assets_dir, info.sources_dir
    );
    if cfg.server.web_root.as_os_str().is_empty() {
        eprintln!("blast server: serving the built-in web app");
    } else {
        eprintln!("blast server: web root {}", cfg.server.web_root.display());
    }
    for u in &urls {
        eprintln!("blast server: listening on {u}");
    }
    if let Err(e) = daemon::write_pid(&cfg.server.pid_file, &urls) {
        eprintln!("Warn: could not write the pid file {e}");
    }

    let pid_file = cfg.server.pid_file.clone();
    let code = rt.block_on(async move {
        // status once a second, for the status bar
        let status_engine = engine.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let st = status_engine.status();
                status_engine.hub.emit(&engine::hub::Event::Status(st));
            }
        });

        let app = server::router(server::App::new(cfg.clone(), engine));
        let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        let mut tasks = Vec::new();
        for l in listeners {
            let mut stop = stop_tx.subscribe();
            let svc = app
                .clone()
                .into_make_service_with_connect_info::<SocketAddr>();
            tasks.push(tokio::spawn(async move {
                // audio goes out in small writes every 20 ms: Nagle would hold
                // them back waiting for the phone's (delayed) ACKs
                let l = l.tap_io(|tcp| {
                    let _ = tcp.set_nodelay(true);
                });
                axum::serve(l, svc)
                    .with_graceful_shutdown(async move {
                        let _ = stop.recv().await;
                    })
                    .await
            }));
        }
        wait_for_stop(detached).await;
        eprintln!("blast server: shutting down");
        let _ = stop_tx.send(());
        let mut code = ExitCode::SUCCESS;
        for t in tasks {
            if let Ok(Err(e)) = t.await {
                eprintln!("Error: {e}");
                code = ExitCode::FAILURE;
            }
        }
        code
    });
    daemon::remove_pid(&pid_file);
    code
}

/// Waits for SIGINT or SIGTERM. A detached server also outlives SIGHUP.
async fn wait_for_stop(detached: bool) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut hup = detached.then(|| signal(SignalKind::hangup()).expect("SIGHUP handler"));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return,
            _ = term.recv() => return,
            _ = async { hup.as_mut().unwrap().recv().await }, if hup.is_some() => continue,
        }
    }
}

/// Listens on `host:port` (required) and every `also_bind` address (best
/// effort).
async fn bind_all(cfg: &config::Config) -> Result<Vec<tokio::net::TcpListener>, String> {
    let main = cfg.server.bind();
    let mut listeners = vec![
        tokio::net::TcpListener::bind(&main)
            .await
            .map_err(|e| format!("could not listen on {main}: {e}"))?,
    ];
    for a in &cfg.server.also_bind {
        let addr = match resolve_bind(a) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Warn: also_bind {a}: {e}");
                continue;
            }
        };
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => listeners.push(l),
            Err(e) => eprintln!("Warn: could not listen on {addr}: {e}"),
        }
    }
    Ok(listeners)
}

/// `tailscale:PORT` becomes this machine's tailnet IPv4 and PORT.
fn resolve_bind(a: &str) -> Result<String, String> {
    let Some(port) = a.strip_prefix("tailscale:") else {
        return Ok(a.to_string());
    };
    let out = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .map_err(|e| format!("tailscale ip: {e}"))?;
    let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if ip.is_empty() {
        return Err("tailscale ip -4 printed nothing (is tailscale up?)".into());
    }
    Ok(format!("{ip}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, lexopt::Error> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn flags_and_commands() {
        let a = parse(&["--bg", "--port", "9000", "--host", "0.0.0.0"]).unwrap();
        assert_eq!(a.cmd, None);
        assert!(a.background);
        assert_eq!(a.port, Some(9000));
        assert_eq!(a.host.as_deref(), Some("0.0.0.0"));
        assert_eq!(parse(&["stop"]).unwrap().cmd, Some(Cmd::Stop));
        assert_eq!(parse(&["status"]).unwrap().cmd, Some(Cmd::Status));
        let a = parse(&["start", "--script", "demo6"]).unwrap();
        assert_eq!(a.cmd, Some(Cmd::Start));
        assert!(!a.background);
        assert_eq!(a.script.as_deref(), Some("demo6"));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&["--port", "http"]).is_err());
        assert!(parse(&["--port", "70000"]).is_err());
        assert!(parse(&["restart"]).is_err());
        assert!(parse(&["stop", "status"]).is_err());
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--fg"]).is_err());
    }

    #[test]
    fn flags_override_the_file() {
        let dir = std::env::temp_dir().join(format!("blast-server-args-{}", std::process::id()));
        let path = dir.join("server.toml");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "[server]\nport = 9100\n").unwrap();
        let mut a = parse(&["--port", "9200", "--device", "none"]).unwrap();
        a.config = Some(path.clone());
        let cfg = load_config(&a).unwrap();
        assert_eq!(cfg.server.port, 9200);
        assert_eq!(cfg.engine.device, "none");
        let mut a = parse(&[]).unwrap();
        a.config = Some(path);
        let cfg = load_config(&a).unwrap();
        assert_eq!(cfg.server.port, 9100);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
