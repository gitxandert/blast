//! The engine host: one BLAST session (assets, engine, command processor)
//! at a time, an output it plays on, and the browser tap. A supervisor
//! thread owns the session's lifecycle (boot, restart, `quit`), so every
//! lifecycle change is serialized.

pub mod hub;
pub mod output;
pub mod pipewire;
pub mod session;
pub mod stream;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use blast::audio_processing::blast_time::sample_rate;
use blast::audio_processing::commands::{CmdProcessor, EngineState, cmd_queue, inspect::NameKind};
use blast::audio_processing::engine::Conductor;
use blast::audio_processing::runtime::audio;
use blast::file_parsing::{load_assets, preferred_config, scan_assets};
use serde::Serialize;
use tokio::sync::{broadcast, oneshot};

use crate::config::Config;
use hub::{Event, Hub};
use output::{Core, Output, OutputInfo, Shared};
use session::{Ended, HistKey, Req, Session};

/// The session renders stereo whatever the device: the browser stream is
/// stereo, and the output maps it onto the device's channels.
pub const CHANNELS: usize = 2;

/// The mirror and the engine for `files`. They share each track's samples
/// (one allocation); track IDs follow the mirror's name order.
fn build_engine(
    files: Vec<blast::file_parsing::decode_helpers::AudioFile>,
) -> (EngineState, Conductor, Vec<String>) {
    let names: Vec<String> = files.iter().map(|f| f.file_name.clone()).collect();
    let engine_state = EngineState::new(files, CHANNELS);
    let conductor = Conductor::prepare_shared(CHANNELS, engine_state.shared_tracks());
    (engine_state, conductor, names)
}

/// What browsers show about the running session.
#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    pub rate: u32,
    pub channels: usize,
    pub assets_dir: String,
    pub sources_dir: String,
    /// Bumped on every new session.
    pub generation: u32,
    pub started_at: u64,
    pub output: OutputInfo,
}

enum Sup {
    Restart {
        keep_state: bool,
        /// Open this output after the new session is in (switching to a
        /// device at another rate).
        device: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// The session thread returned after `quit`.
    Quit,
}

pub struct Engine {
    pub cfg: Arc<Config>,
    pub hub: Arc<Hub>,
    pub shared: Arc<Shared>,
    pub output: Output,
    pub audio: broadcast::Sender<Bytes>,
    pub listeners: AtomicUsize,
    session: Mutex<mpsc::Sender<Req>>,
    info: Mutex<EngineInfo>,
    sup: mpsc::Sender<Sup>,
}

impl Engine {
    /// Loads the assets, opens the configured output and starts the first
    /// session. A device that can't open leaves the engine on the software
    /// clock, with a notice, rather than failing.
    pub fn start(cfg: Arc<Config>) -> Result<Arc<Engine>, String> {
        let hub = Hub::new(cfg.server.scrollback);
        let (shared, tap) = Shared::new();
        shared
            .period
            .store(cfg.engine.period_frames, Ordering::Relaxed);
        let fail_hub = hub.clone();
        let output = Output::start(
            shared.clone(),
            cfg.engine.fallback_to_none,
            move |why, now| {
                let msg = match now {
                    Some(o) => format!("Warn: {why}; now playing on: {}", o.name),
                    None => format!("Warn: {why}; output stopped"),
                };
                fail_hub.print("warn", &msg, None);
                fail_hub.emit(&Event::Engine);
            },
        );
        let (audio_tx, _) = broadcast::channel(64);
        stream::start(
            shared.clone(),
            tap,
            audio_tx.clone(),
            cfg.stream.chunk_ms,
            stream::WireFormat::parse(&cfg.stream.format),
        );

        let (sup_tx, sup_rx) = mpsc::channel();
        let (dummy_tx, _) = mpsc::channel();
        let engine = Arc::new(Engine {
            cfg: cfg.clone(),
            hub,
            shared,
            output,
            audio: audio_tx,
            listeners: AtomicUsize::new(0),
            session: Mutex::new(dummy_tx),
            info: Mutex::new(EngineInfo {
                rate: 0,
                channels: CHANNELS,
                assets_dir: cfg.engine.assets_dir.display().to_string(),
                sources_dir: cfg.engine.sources_dir.display().to_string(),
                generation: 0,
                started_at: 0,
                output: OutputInfo::clock(48_000),
            }),
            sup: sup_tx,
        });

        let device = Some(cfg.start_device()).filter(|d| d != "none");
        engine.boot(None, device, None, cfg.script())?;

        let e = engine.clone();
        thread::Builder::new()
            .name("blast-server-supervisor".into())
            .spawn(move || {
                while let Ok(msg) = sup_rx.recv() {
                    match msg {
                        Sup::Restart {
                            keep_state,
                            device,
                            reply,
                        } => {
                            let _ = reply.send(e.restart(keep_state, device));
                        }
                        Sup::Quit => {
                            e.hub.print(
                                "info",
                                "Session ended (quit). Starting a fresh session.",
                                None,
                            );
                            let device = e.output.info().id;
                            let device = Some(device).filter(|d| d != "none");
                            if let Err(err) = e.boot(None, device, None, None) {
                                e.hub.print(
                                    "err",
                                    &format!("Err: could not start a session: {err}"),
                                    None,
                                );
                            }
                        }
                    }
                }
            })
            .expect("spawn supervisor");
        Ok(engine)
    }

    /// Builds a session (at `rate`, else the device's rate for the assets'
    /// preferred one), puts its engine in, opens `device`, restores
    /// `state`, runs `script`, and starts the session thread.
    fn boot(
        &self,
        rate: Option<u32>,
        device: Option<String>,
        state: Option<String>,
        script: Option<&str>,
    ) -> Result<(), String> {
        let ecfg = &self.cfg.engine;
        let dir = ecfg.assets_dir.display().to_string();
        let assets = scan_assets(&ecfg.assets_dir)
            .map_err(|e| format!("could not load assets from '{dir}': {e}"))?;
        let (want_rate, _) = preferred_config(&assets)
            .ok_or_else(|| format!("no playable audio files in '{dir}'"))?;

        // the rate: asked for, else what the device runs closest to ours
        let mut device = device;
        let rate = match (rate, device.as_deref()) {
            (Some(r), _) => r,
            (None, None) => want_rate,
            (None, Some(id)) => match device_rate(id, want_rate) {
                Ok(r) => r,
                Err(e) => {
                    self.hub.print(
                        "warn",
                        &format!("Warn: output '{id}': {e}; playing on no local output"),
                        None,
                    );
                    device = None;
                    want_rate
                }
            },
        };

        let files = load_assets(&assets, rate, CHANNELS as u32)
            .map_err(|e| format!("could not load assets from '{dir}': {e}"))?;
        sample_rate::set(rate);
        let (mut engine_state, mut conductor, names) = build_engine(files);
        engine_state.set_assets_dir(Some(ecfg.assets_dir.clone()));
        let rx = conductor.take_rx().ok_or("engine queues already taken")?;
        let (mut tx, cmd_rx) = cmd_queue(256);
        let mut processor = CmdProcessor::new(engine_state);
        processor.set_sources_dir(ecfg.sources_dir.clone());
        let _ = tx.try_push(processor.boot());

        let old = self.shared.swap_core(
            Some(Core {
                conductor,
                commands: cmd_rx,
                channels: CHANNELS,
            }),
            rate,
        );
        drop(old);

        // (re)open the output: a new rate needs a new stream
        let out = self.output.open(device.as_deref()).or_else(|e| {
            self.hub.print(
                "warn",
                &format!("Warn: {e}; playing on no local output"),
                None,
            );
            self.output.open(None)
        })?;

        self.hub.print(
            "info",
            &format!(
                "Loaded tracks [{}]\nOutput: {} · {} Hz · {} ch · {} · buffer {}",
                names.join(", "),
                out.name,
                rate,
                out.channels,
                out.format,
                out.buffer
            ),
            None,
        );

        if let Some(text) = state.filter(|s| !s.trim().is_empty()) {
            match processor.run_text("restored session", &text, &mut tx) {
                Ok(_) => self
                    .hub
                    .print("info", "Session restored from its state.", None),
                Err(e) => self.hub.print(
                    "err",
                    &format!("Err: could not restore the session: {e}"),
                    None,
                ),
            }
            processor.history_out.clear();
        }
        if let Some(script) = script {
            let path = processor.resolve_script_arg(Path::new(script));
            match processor.run_script_text(&path, &mut tx) {
                Ok(text) if text.is_empty() => {
                    self.hub
                        .print("info", &format!("Ran script {script}."), None)
                }
                Ok(text) => self.hub.print("out", &text, None),
                Err(e) => self.hub.print("err", &format!("Err: {e}"), None),
            }
            processor.history_out.clear();
        }

        let session = Session::new(processor, tx, rx, self.hub.clone());
        let (req_tx, req_rx) = mpsc::channel();
        *self.session.lock().unwrap() = req_tx;
        let sup = self.sup.clone();
        thread::Builder::new()
            .name("blast-server-session".into())
            .spawn(move || {
                if session.run(req_rx) == Ended::Quit {
                    let _ = sup.send(Sup::Quit);
                }
            })
            .map_err(|e| e.to_string())?;

        {
            let mut info = self.info.lock().unwrap();
            info.rate = rate;
            info.generation += 1;
            info.started_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            info.output = out;
        }
        self.hub.prompt(false);
        self.hub.emit(&Event::Engine);
        Ok(())
    }

    fn restart(&self, keep_state: bool, device: Option<String>) -> Result<(), String> {
        let state = if keep_state {
            let (tx, rx) = oneshot::channel();
            self.send(Req::State { reply: tx });
            rx.blocking_recv().ok()
        } else {
            None
        };
        let (tx, rx) = oneshot::channel();
        self.send(Req::End { reply: tx });
        let _ = rx.blocking_recv();
        let device = device.or_else(|| Some(self.output.info().id));
        let device = device.filter(|d| d != "none");
        self.hub.print("info", "Restarting the engine…", None);
        self.boot(None, device, state, None)
    }

    fn send(&self, req: Req) {
        let _ = self.session.lock().unwrap().send(req);
    }

    // ---- what the server calls ----

    pub fn info(&self) -> EngineInfo {
        let mut i = self.info.lock().unwrap().clone();
        i.output = self.output.info();
        i
    }

    pub fn submit(&self, line: String, user: String, hist: HistKey) {
        self.send(Req::Submit { line, user, hist });
    }

    pub fn cancel(&self) {
        self.send(Req::Cancel);
    }

    pub async fn complete(&self, line: String, cursor: usize) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        self.send(Req::Complete {
            line,
            cursor,
            reply: tx,
        });
        rx.await.unwrap_or_default()
    }

    pub async fn history(&self, hist: HistKey) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        self.send(Req::History { hist, reply: tx });
        rx.await.unwrap_or_default()
    }

    pub async fn check(&self, name: String, text: String) -> (Vec<String>, Vec<String>) {
        let (tx, rx) = oneshot::channel();
        self.send(Req::Check {
            name,
            text,
            reply: tx,
        });
        rx.await.unwrap_or_default()
    }

    pub async fn tracks(&self) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        self.send(Req::Tracks { reply: tx });
        rx.await.unwrap_or_default()
    }

    /// Restarts the session (keeping its state as a script if asked) and
    /// optionally moves to `device` once the new session is in.
    pub async fn restart_async(
        &self,
        keep_state: bool,
        device: Option<String>,
    ) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.sup
            .send(Sup::Restart {
                keep_state,
                device,
                reply: tx,
            })
            .map_err(|_| "supervisor is gone".to_string())?;
        rx.await.map_err(|_| "supervisor is gone".to_string())?
    }

    /// Moves the output to `id` (`none`: browser only). A device that can't
    /// run at the session's rate is refused unless `restart` is set, which
    /// restarts the session at the device's rate, keeping its state.
    pub async fn set_output(
        self: &Arc<Self>,
        id: String,
        restart: bool,
    ) -> Result<OutputInfo, String> {
        let e = self.clone();
        let target = id.clone();
        let result = tokio::task::spawn_blocking(move || e.output.open(Some(&target)))
            .await
            .map_err(|e| e.to_string())?;
        match result {
            Ok(info) => {
                self.cfg.remember_output(&info.id);
                self.hub.print(
                    "info",
                    &format!(
                        "Output: {} · {} Hz · {} ch",
                        info.name, info.rate, info.channels
                    ),
                    None,
                );
                self.hub.emit(&Event::Engine);
                Ok(info)
            }
            Err(e) if restart && e.contains("can't run at") => {
                self.restart_async(true, Some(id)).await?;
                let info = self.output.info();
                self.cfg.remember_output(&info.id);
                Ok(info)
            }
            Err(e) => Err(e),
        }
    }

    /// A browser started or stopped listening.
    pub fn listen(&self, on: bool) {
        let n = if on {
            self.listeners.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            self.listeners
                .fetch_sub(1, Ordering::Relaxed)
                .saturating_sub(1)
        };
        self.shared.tap_on.store(n > 0, Ordering::Relaxed);
    }

    pub fn status(&self) -> serde_json::Value {
        let st = &self.shared.stats;
        let out = self.output.info();
        serde_json::json!({
            "xruns": st.xruns.load(Ordering::Relaxed),
            "period_frames": st.period_frames.load(Ordering::Relaxed),
            "latency_ms": f64::from(st.latency_us.load(Ordering::Relaxed)) / 1000.0,
            "listeners": self.listeners.load(Ordering::Relaxed),
            "output": out.name,
            "output_id": out.id,
            "rate": self.shared.rate.load(Ordering::Relaxed),
            "pending": self.hub.pending(),
        })
    }
}

/// The rate `id` would run at when asked for `want`.
pub fn device_rate(id: &str, want: u32) -> Result<u32, String> {
    let name = if id == "default" { None } else { Some(id) };
    let device = audio::open_device(name).map_err(|e| e.to_string())?;
    let out = audio::choose_config(&device, want, CHANNELS as u32).map_err(|e| e.to_string())?;
    Ok(out.config.sample_rate)
}

/// The names of the session's tracks, for `Req::Tracks`.
pub fn track_names(p: &CmdProcessor) -> Vec<String> {
    p.names(NameKind::Track)
}

#[cfg(test)]
mod build_tests {
    use super::*;
    use blast::file_parsing::decode_helpers::AudioFile;

    fn tone(name: &str, frames: usize) -> AudioFile {
        AudioFile::new(
            name,
            "wav",
            48_000,
            CHANNELS as u32,
            16,
            vec![0.25; frames * CHANNELS],
        )
    }

    #[test]
    fn engine_shares_startup_tracks_with_the_mirror() {
        let (es, mut conductor, names) = build_engine(vec![tone("zed", 64), tone("abc", 32)]);
        assert_eq!(names, ["zed", "abc"]);
        assert!(conductor.take_rx().is_some(), "the REPL ends are kept");
        // the conductor's arena is crate-private: count owners instead.
        // mirror + engine + this clone = 3 if shared; 2 if the engine copied.
        let held = es.shared_tracks();
        assert_eq!(held.len(), 2);
        assert_eq!(held[0].frames, 32, "ids follow name order");
        assert_eq!(held[1].frames, 64);
        for t in &held {
            assert_eq!(std::sync::Arc::strong_count(&t.samples), 3);
        }
    }
}
