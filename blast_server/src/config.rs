//! Configuration: one TOML file for the server and the engine, `[defaults]`
//! for every user's preferences, `[users."login"]` to override them per
//! user, and a per-user file under the state dir that the browser writes.
//!
//! The file lives at `$XDG_CONFIG_HOME/blast/server.toml` (else
//! `~/.config/blast/server.toml`). When it doesn't exist, the server writes
//! it with every key at its default value ([`default_file`]), so the file
//! always shows what can be set. Command-line flags override the file.
//!
//! Precedence for a user's preferences, lowest first: built-in defaults,
//! `[defaults]`, `[users."<login>"]`, `<state_dir>/users/<slug>.toml`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The whole config file.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub engine: EngineConfig,
    pub stream: StreamConfig,
    /// Preferences every user starts from.
    pub defaults: Prefs,
    /// Per-user overrides, keyed by Tailscale login (or `local`).
    pub users: BTreeMap<String, UserConfig>,
    /// Where the file was read from (not in the file).
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Address to listen on (`--host`). Keep it on loopback and put
    /// `tailscale serve` in front: Tailscale terminates HTTPS (phones need a
    /// secure context for audio worklets) and tells us who is asking.
    pub host: String,
    /// Port to listen on (`--port`).
    pub port: u16,
    /// Where a background server (`--bg`) writes its output (`--log`).
    pub log_file: PathBuf,
    /// Holds the running server's pid; `blast server stop` and `status`
    /// read it.
    pub pid_file: PathBuf,
    /// The browser app's files; empty serves the copy built into the binary.
    pub web_root: PathBuf,
    /// Per-user preference files and history live here.
    pub state_dir: PathBuf,
    /// Tailscale logins allowed in; empty lets in everyone who reaches the
    /// server. `local` is a request without Tailscale headers (loopback).
    pub allowed_users: Vec<String>,
    /// More addresses to serve on, e.g. `tailscale:8800` (this machine's
    /// tailnet IPv4). Requests there are plain HTTP straight from the
    /// tailnet (WireGuard-encrypted); the user is found with `tailscale
    /// whois`, and Tailscale headers are ignored. Without HTTPS a browser
    /// plays through a ScriptProcessor instead of an AudioWorklet.
    pub also_bind: Vec<String>,
    /// Largest upload accepted, in MiB.
    pub max_upload_mb: u64,
    /// Lines of transcript a newly connected browser receives.
    pub scrollback: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let state = default_state_dir();
        ServerConfig {
            host: "127.0.0.1".into(),
            port: 8800,
            log_file: state.join("server.log"),
            pid_file: state.join("server.pid"),
            web_root: PathBuf::new(),
            state_dir: state,
            allowed_users: Vec::new(),
            also_bind: Vec::new(),
            max_upload_mb: 200,
            scrollback: 2000,
        }
    }
}

impl ServerConfig {
    /// `host:port`, bracketing an IPv6 host.
    pub fn bind(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineConfig {
    /// Audio files to load as tracks (`--assets`; blast's ASSETS_DIR).
    pub assets_dir: PathBuf,
    /// `source NAME` / `state to NAME` folder (`--sources`).
    pub sources_dir: PathBuf,
    /// Output device at startup (`--device`): a cpal id (`alsa:pipewire`), a
    /// device name, `default`, or `none` (no local output: the browser is
    /// the only listener and a software clock drives the engine).
    pub device: String,
    /// A script run once the engine is up (`--script`); empty runs none.
    pub script: String,
    /// History file: `blast` shares the terminal REPL's file
    /// (`~/.local/state/blast/history`), `per-user` keeps one per user in
    /// the state dir, `off` keeps it in memory only.
    pub history: String,
    /// When the device stream fails (a USB device unplugged), keep the
    /// engine running on the software clock instead of stopping.
    pub fallback_to_none: bool,
    /// Start on the output last picked in a browser (kept in the state
    /// dir) instead of `device`.
    pub remember_output: bool,
    /// Add PipeWire sinks to the device list (moves just this stream with
    /// `pw-metadata`; needs pw-dump and pw-metadata).
    pub pipewire_routing: bool,
    /// Frames per device period (blast's terminal default too). Through a
    /// sound server it sets the quantum; on a direct USB device a short
    /// two-period ring can run slow (see the README, "Output devices from
    /// the browser").
    pub period_frames: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        // the same folders `blast` uses when given none
        let blast = Path::new(env!("CARGO_MANIFEST_DIR")).join("../blast");
        let blast = blast.canonicalize().unwrap_or(blast);
        EngineConfig {
            assets_dir: blast.join("assets"),
            sources_dir: blast.join("sources"),
            device: "default".into(),
            script: String::new(),
            history: "blast".into(),
            fallback_to_none: true,
            remember_output: true,
            pipewire_routing: true,
            period_frames: 128,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct StreamConfig {
    /// Samples on the wire: `i16` (half the bandwidth) or `f32`.
    pub format: String,
    /// Audio per WebSocket message, in milliseconds.
    pub chunk_ms: u32,
    /// Most browsers listening at once.
    pub max_listeners: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        StreamConfig {
            format: "i16".into(),
            chunk_ms: 20,
            max_listeners: 8,
        }
    }
}

/// What a user can change from the browser. Every field is optional so a
/// layer only states what it overrides.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Prefs {
    /// `dark`, `light` or `auto`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Transcript and input font size, px.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<u32>,
    /// Audio the browser holds before playing (absorbs network jitter), ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<u32>,
    /// Listening volume, 0.0..=1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    /// Start listening as soon as the page is tapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_listen: Option<bool>,
    /// Keys on the symbol bar above the input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_keys: Option<Vec<String>>,
    /// Run `rescan` after an asset upload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rescan_after_upload: Option<bool>,
    /// Animate the prompt marker like the terminal does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<bool>,
    /// Show completion candidates while typing (not only on Tab).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_complete: Option<bool>,
}

impl Prefs {
    /// The built-in values under every layer.
    pub fn builtin() -> Prefs {
        Prefs {
            theme: Some("auto".into()),
            font_size: Some(14),
            jitter_ms: Some(120),
            volume: Some(0.9),
            auto_listen: Some(false),
            symbol_keys: Some(
                ["{", "}", "|", "=", "\"", ";", "&", "#", ".", "%", "(", ")"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            rescan_after_upload: Some(true),
            marker: Some(true),
            live_complete: Some(true),
        }
    }

    /// `self` with every field `over` states replaced.
    pub fn layered(mut self, over: &Prefs) -> Prefs {
        macro_rules! take {
            ($($f:ident),*) => { $( if over.$f.is_some() { self.$f = over.$f.clone(); } )* };
        }
        take!(
            theme,
            font_size,
            jitter_ms,
            volume,
            auto_listen,
            symbol_keys,
            rescan_after_upload,
            marker,
            live_complete
        );
        self
    }

    /// Clamps values the browser sent into sane ranges.
    pub fn sanitized(mut self) -> Prefs {
        if let Some(t) = &self.theme
            && !matches!(t.as_str(), "dark" | "light" | "auto")
        {
            self.theme = None;
        }
        self.font_size = self.font_size.map(|f| f.clamp(9, 32));
        self.jitter_ms = self.jitter_ms.map(|j| j.clamp(20, 2000));
        self.volume = self.volume.map(|v| v.clamp(0.0, 1.0));
        if let Some(keys) = &mut self.symbol_keys {
            keys.retain(|k| !k.is_empty() && k.chars().count() <= 8);
            keys.truncate(32);
        }
        self
    }
}

/// `[users."login"]`: `display_name` plus any preference. (Not
/// `#[serde(flatten)]`: serde can't flatten and deny unknown fields, and a
/// typo in a preference should be an error.)
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(try_from = "toml::Table", into = "toml::Table")]
pub struct UserConfig {
    /// Shown in the app instead of the login.
    pub display_name: Option<String>,
    pub prefs: Prefs,
}

impl TryFrom<toml::Table> for UserConfig {
    type Error = String;
    fn try_from(mut t: toml::Table) -> Result<Self, String> {
        let display_name = match t.remove("display_name") {
            None => None,
            Some(toml::Value::String(s)) => Some(s),
            Some(_) => return Err("display_name must be a string".into()),
        };
        let prefs = toml::Value::Table(t)
            .try_into()
            .map_err(|e: toml::de::Error| e.to_string())?;
        Ok(UserConfig {
            display_name,
            prefs,
        })
    }
}

impl From<UserConfig> for toml::Table {
    fn from(u: UserConfig) -> toml::Table {
        let mut t = toml::Table::try_from(&u.prefs).unwrap_or_default();
        if let Some(n) = u.display_name {
            t.insert("display_name".into(), toml::Value::String(n));
        }
        t
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `$XDG_STATE_HOME/blast/server`, else `~/.local/state/blast/server`.
fn default_state_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(x).join("blast/server");
    }
    match home() {
        Some(h) => h.join(".local/state/blast/server"),
        None => PathBuf::from(".blast-server-state"),
    }
}

/// `$XDG_CONFIG_HOME/blast/server.toml`, else `~/.config/blast/server.toml`.
pub fn default_path() -> Option<PathBuf> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) => PathBuf::from(x),
        None => home()?.join(".config"),
    };
    Some(dir.join("blast/server.toml"))
}

/// A TOML string literal.
fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn qp(p: &Path) -> String {
    q(&p.display().to_string())
}

/// The config file with every key at its default value, and what each
/// one does. Written where the config belongs when none exists yet.
pub fn default_file() -> String {
    let c = Config::default();
    let (s, e, st, p) = (&c.server, &c.engine, &c.stream, Prefs::builtin());
    let keys: Vec<String> = p
        .symbol_keys
        .unwrap_or_default()
        .iter()
        .map(|k| q(k))
        .collect();
    format!(
        r#"# blast server configuration: every key, at its default value.
# Flags given to `blast server` override this file (`blast server --help`).
# Relative paths are taken from this file's folder; `~/` is your home.

[server]
# Address and port to listen on (--host, --port). Keep it on loopback and
# put `tailscale serve` in front: it adds HTTPS (phones only get the
# low-latency audio player in a secure context) and the header that tells
# the server who is asking.
host = {host}
port = {port}
# Where a server started with --bg writes its output (--log).
log_file = {log_file}
# The running server's pid, for `blast server stop` and `status`.
pid_file = {pid_file}
# The browser app's files; "" serves the copy built into blast-server.
web_root = {web_root}
# Per-user preference files (and per-user history) go here.
state_dir = {state_dir}
# Tailscale logins allowed in; empty = anyone who can reach the server.
# "local" is a request that didn't come through Tailscale (loopback).
allowed_users = []
# Also serve plain HTTP on the tailnet, for when `tailscale serve` isn't
# set up ("tailscale:PORT" = this machine's tailnet IPv4). Users are
# identified with `tailscale whois`.
also_bind = []
# Largest upload accepted, in MiB.
max_upload_mb = {max_upload_mb}
# Transcript lines a newly connected browser receives.
scrollback = {scrollback}

[engine]
# Audio files loaded as tracks (--assets).
assets_dir = {assets_dir}
# Folder for `source NAME` and `state to NAME` (--sources).
sources_dir = {sources_dir}
# Output device (--device): a cpal id or name from `blast --list-devices`,
# "default", or "none" (browser only: a software clock drives the engine).
device = {device}
# Script run once the engine is up (--script); "" runs none.
script = {script}
# "blast" shares the terminal REPL's history file, "per-user", or "off".
history = {history}
# A failing device (USB unplugged) drops to the software clock instead of
# stopping the session.
fallback_to_none = {fallback_to_none}
# Start on the output last picked in a browser (else `device`).
remember_output = {remember_output}
# List PipeWire sinks too, and move just this stream between them.
pipewire_routing = {pipewire_routing}
# Frames per device period (clamped to what the device allows). Through a
# sound server it sets the quantum. 128 is 2.7 ms at 48 kHz; raise it if the
# device can't keep up (direct USB: see the README).
period_frames = {period_frames}

[stream]
# Samples sent to listening browsers: "i16" or "f32".
format = {format}
# Audio per message, in ms (5-200).
chunk_ms = {chunk_ms}
# Most browsers listening at once.
max_listeners = {max_listeners}

# What every user starts with. Users change these from the Settings panel;
# that writes <state_dir>/users/<login>.toml, which wins over this file.
[defaults]
theme = {theme}                # "dark", "light", "auto"
font_size = {font_size}
jitter_ms = {jitter_ms}               # audio buffered in the browser before playing
volume = {volume}
auto_listen = {auto_listen}
rescan_after_upload = {rescan}
marker = {marker}                 # animate the prompt marker
live_complete = {live}          # completion chips while typing, not only on Tab
symbol_keys = [{symbol_keys}]

# Per-user overrides, keyed by Tailscale login ("local" for loopback): a
# display name and any key from [defaults].
# [users."someone@example.com"]
# display_name = "Someone"
# jitter_ms = 150
"#,
        host = q(&s.host),
        port = s.port,
        log_file = qp(&s.log_file),
        pid_file = qp(&s.pid_file),
        web_root = qp(&s.web_root),
        state_dir = qp(&s.state_dir),
        max_upload_mb = s.max_upload_mb,
        scrollback = s.scrollback,
        assets_dir = qp(&e.assets_dir),
        sources_dir = qp(&e.sources_dir),
        device = q(&e.device),
        script = q(&e.script),
        history = q(&e.history),
        fallback_to_none = e.fallback_to_none,
        remember_output = e.remember_output,
        pipewire_routing = e.pipewire_routing,
        period_frames = e.period_frames,
        format = q(&st.format),
        chunk_ms = st.chunk_ms,
        max_listeners = st.max_listeners,
        theme = q(p.theme.as_deref().unwrap_or("auto")),
        font_size = p.font_size.unwrap_or(14),
        jitter_ms = p.jitter_ms.unwrap_or(120),
        volume = p.volume.unwrap_or(0.9),
        auto_listen = p.auto_listen.unwrap_or(false),
        rescan = p.rescan_after_upload.unwrap_or(true),
        marker = p.marker.unwrap_or(true),
        live = p.live_complete.unwrap_or(true),
        symbol_keys = keys.join(", "),
    )
}

/// Writes [`default_file`] to `path` unless something is there already.
/// Returns whether it wrote.
pub fn ensure_file(path: &Path) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create '{}': {e}", dir.display()))?;
    }
    std::fs::write(path, default_file())
        .map_err(|e| format!("could not write '{}': {e}", path.display()))?;
    Ok(true)
}

/// `~/` expanded, then relative paths taken from `base`. Empty stays empty.
pub fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.as_os_str().is_empty() {
        return PathBuf::new();
    }
    let p = match (p.strip_prefix("~"), home()) {
        (Ok(rest), Some(h)) => h.join(rest),
        _ => p.to_path_buf(),
    };
    if p.is_absolute() { p } else { base.join(p) }
}

impl Config {
    /// Reads `path`, or the defaults when `path` is `None`. Relative paths
    /// in the file are resolved against the file's folder.
    pub fn load(path: Option<&Path>) -> Result<Config, String> {
        let (mut cfg, base) = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| format!("could not read config '{}': {e}", p.display()))?;
                let cfg: Config = toml::from_str(&text)
                    .map_err(|e| format!("bad config '{}': {e}", p.display()))?;
                let base = p
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."));
                (cfg, base)
            }
            None => (
                Config::default(),
                std::env::current_dir().unwrap_or_default(),
            ),
        };
        cfg.path = path.map(Path::to_path_buf);
        cfg.resolve_paths(&base);
        cfg.validate()?;
        Ok(cfg)
    }

    /// Makes every path absolute, relative ones taken from `base`.
    pub fn resolve_paths(&mut self, base: &Path) {
        let s = &mut self.server;
        for p in [
            &mut s.web_root,
            &mut s.state_dir,
            &mut s.log_file,
            &mut s.pid_file,
            &mut self.engine.assets_dir,
            &mut self.engine.sources_dir,
        ] {
            *p = resolve(base, p);
        }
    }

    /// Rejects values the server can't run with; clamps the rest.
    pub fn validate(&mut self) -> Result<(), String> {
        if !matches!(self.stream.format.as_str(), "i16" | "f32") {
            return Err(format!(
                "stream.format must be \"i16\" or \"f32\", not {:?}",
                self.stream.format
            ));
        }
        if !matches!(self.engine.history.as_str(), "blast" | "per-user" | "off") {
            return Err(format!(
                "engine.history must be \"blast\", \"per-user\" or \"off\", not {:?}",
                self.engine.history
            ));
        }
        if self.server.host.is_empty() {
            return Err("server.host is empty".into());
        }
        self.stream.chunk_ms = self.stream.chunk_ms.clamp(5, 200);
        Ok(())
    }

    /// The script to run at startup, if any.
    pub fn script(&self) -> Option<&str> {
        Some(self.engine.script.as_str()).filter(|s| !s.is_empty())
    }

    pub fn user_allowed(&self, login: &str) -> bool {
        self.server.allowed_users.is_empty() || self.server.allowed_users.iter().any(|u| u == login)
    }

    /// The user's file of browser-saved preferences.
    pub fn prefs_path(&self, login: &str) -> PathBuf {
        self.server
            .state_dir
            .join("users")
            .join(format!("{}.toml", slug(login)))
    }

    /// The preferences `login` sees: every layer applied.
    pub fn prefs_for(&self, login: &str) -> Prefs {
        let mut p = Prefs::builtin().layered(&self.defaults);
        if let Some(u) = self.users.get(login) {
            p = p.layered(&u.prefs);
        }
        p.layered(&self.saved_prefs(login))
    }

    /// What the browser saved for `login` (empty when nothing was).
    pub fn saved_prefs(&self, login: &str) -> Prefs {
        std::fs::read_to_string(self.prefs_path(login))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Merges `changes` into what the browser saved for `login`.
    pub fn save_prefs(&self, login: &str, changes: &Prefs) -> Result<Prefs, String> {
        let merged = self
            .saved_prefs(login)
            .layered(&changes.clone().sanitized());
        let path = self.prefs_path(login);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(&merged).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        Ok(self.prefs_for(login))
    }

    /// Drops what the browser saved for `login`.
    pub fn reset_prefs(&self, login: &str) -> Prefs {
        let _ = std::fs::remove_file(self.prefs_path(login));
        self.prefs_for(login)
    }

    /// Where the last output picked in a browser is kept.
    pub fn last_output_path(&self) -> PathBuf {
        self.server.state_dir.join("last-output")
    }

    /// The output to start on: the remembered one, else `engine.device`.
    pub fn start_device(&self) -> String {
        if self.engine.remember_output
            && let Ok(id) = std::fs::read_to_string(self.last_output_path())
            && !id.trim().is_empty()
        {
            return id.trim().to_string();
        }
        self.engine.device.clone()
    }

    pub fn remember_output(&self, id: &str) {
        if !self.engine.remember_output {
            return;
        }
        let p = self.last_output_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, id);
    }

    pub fn display_name(&self, login: &str) -> String {
        self.users
            .get(login)
            .and_then(|u| u.display_name.clone())
            .unwrap_or_else(|| login.split('@').next().unwrap_or(login).to_string())
    }

    /// The history file for `login`, or `None` for an in-memory history.
    pub fn history_path(&self, login: &str) -> Option<PathBuf> {
        match self.engine.history.as_str() {
            "off" => None,
            "per-user" => Some(
                self.server
                    .state_dir
                    .join("users")
                    .join(format!("{}.history", slug(login))),
            ),
            _ => blast::audio_processing::runtime::history::history_path(
                std::env::var_os("XDG_STATE_HOME").as_deref(),
                std::env::var_os("HOME").as_deref(),
            ),
        }
    }
}

/// A login as a file name: letters, digits, `-`, `_`, `.`; the rest `_`.
pub fn slug(login: &str) -> String {
    let s: String = login
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect();
    match s.trim_start_matches('.') {
        "" => "_".into(),
        t => t.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_override_in_order() {
        let mut cfg = Config::default();
        cfg.defaults.font_size = Some(16);
        cfg.users.insert(
            "a@b".into(),
            UserConfig {
                display_name: Some("A".into()),
                prefs: Prefs {
                    jitter_ms: Some(300),
                    ..Prefs::default()
                },
            },
        );
        let p = cfg.prefs_for("a@b");
        assert_eq!(p.font_size, Some(16));
        assert_eq!(p.jitter_ms, Some(300));
        assert_eq!(p.theme.as_deref(), Some("auto"));
        assert_eq!(cfg.prefs_for("other").jitter_ms, Some(120));
        assert_eq!(cfg.display_name("a@b"), "A");
        assert_eq!(cfg.display_name("x@y.com"), "x");
    }

    #[test]
    fn slugs_are_file_names() {
        assert_eq!(slug("alex@example.com"), "alex_example.com");
        assert_eq!(slug("../etc/passwd"), "_etc_passwd");
        assert_eq!(slug(""), "_");
    }

    /// The written file states every key, each at its default.
    #[test]
    fn default_file_is_the_defaults() {
        let text = default_file();
        let parsed: Config = toml::from_str(&text).unwrap();
        let want = Config {
            defaults: Prefs::builtin(),
            ..Config::default()
        };
        let as_toml = |c: &Config| toml::Table::try_from(c).unwrap();
        assert_eq!(as_toml(&parsed), as_toml(&want));
        // every key of every section is in the file, not left to defaults
        let raw: toml::Table = toml::from_str(&text).unwrap();
        for (section, keys) in as_toml(&want) {
            let Some(keys) = keys.as_table().filter(|k| !k.is_empty()) else {
                continue;
            };
            let written = raw[&section].as_table().unwrap();
            for k in keys.keys() {
                assert!(written.contains_key(k), "[{section}] {k} missing");
            }
        }
    }

    #[test]
    fn ensure_file_writes_once() {
        let dir = std::env::temp_dir().join(format!("blast-server-cfg-{}", std::process::id()));
        let path = dir.join("sub/server.toml");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ensure_file(&path).unwrap());
        std::fs::write(&path, "[server]\nport = 9000\n").unwrap();
        assert!(!ensure_file(&path).unwrap());
        let cfg = Config::load(Some(&path)).unwrap();
        assert_eq!(cfg.server.port, 9000);
        assert_eq!(cfg.server.host, "127.0.0.1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_resolve_from_the_file() {
        let mut cfg = Config::default();
        cfg.server.log_file = "logs/s.log".into();
        cfg.server.web_root = PathBuf::new();
        cfg.resolve_paths(Path::new("/etc/blast"));
        assert_eq!(cfg.server.log_file, PathBuf::from("/etc/blast/logs/s.log"));
        assert_eq!(cfg.server.web_root, PathBuf::new());
    }

    #[test]
    fn bind_brackets_ipv6() {
        let mut s = ServerConfig::default();
        assert_eq!(s.bind(), "127.0.0.1:8800");
        s.host = "::1".into();
        assert_eq!(s.bind(), "[::1]:8800");
    }

    #[test]
    fn validate_rejects_bad_values() {
        let mut cfg = Config::default();
        cfg.engine.history = "nope".into();
        assert!(cfg.validate().is_err());
        let mut cfg = Config::default();
        cfg.stream.chunk_ms = 1000;
        cfg.validate().unwrap();
        assert_eq!(cfg.stream.chunk_ms, 200);
    }

    #[test]
    fn sanitize_clamps() {
        let p = Prefs {
            theme: Some("neon".into()),
            font_size: Some(100),
            volume: Some(3.0),
            ..Prefs::default()
        }
        .sanitized();
        assert_eq!(p.theme, None);
        assert_eq!(p.font_size, Some(32));
        assert_eq!(p.volume, Some(1.0));
    }
}
