//! Fan-out to browsers: every event is serialized once and broadcast; the
//! transcript part of it is kept as scrollback for browsers that connect
//! (or reconnect) later. Every browser sees the same transcript, like one
//! terminal with several keyboards.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::broadcast;

/// Server → browser messages (JSON, tagged by `t`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Event {
    /// Transcript text. `kind`: `echo` (a submitted line), `out`, `err`,
    /// `warn`, `info` (server notices).
    Print {
        kind: String,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        cont: bool,
    },
    /// The prompt: `pending` while a block is open.
    Prompt { pending: bool },
    /// `clear`.
    Clear,
    /// Entries added to (or, with `replace`, the whole of) history `key`.
    History {
        key: String,
        entries: Vec<String>,
        replace: bool,
    },
    /// Engine or output changed: browsers refetch `/api/engine`.
    Engine,
    /// Once a second.
    Status(serde_json::Value),
}

pub struct Hub {
    tx: broadcast::Sender<Arc<str>>,
    scrollback: Mutex<VecDeque<Arc<str>>>,
    cap: usize,
    pending: AtomicBool,
}

impl Hub {
    pub fn new(cap: usize) -> Arc<Hub> {
        let (tx, _) = broadcast::channel(1024);
        Arc::new(Hub {
            tx,
            scrollback: Mutex::new(VecDeque::new()),
            cap: cap.max(10),
            pending: AtomicBool::new(false),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }

    /// The transcript so far, oldest first.
    pub fn scrollback(&self) -> Vec<Arc<str>> {
        self.scrollback.lock().unwrap().iter().cloned().collect()
    }

    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed)
    }

    pub fn emit(&self, event: &Event) {
        let json: Arc<str> = match serde_json::to_string(event) {
            Ok(s) => s.into(),
            Err(_) => return,
        };
        match event {
            Event::Print { .. } => {
                let mut sb = self.scrollback.lock().unwrap();
                sb.push_back(json.clone());
                while sb.len() > self.cap {
                    sb.pop_front();
                }
            }
            Event::Clear => self.scrollback.lock().unwrap().clear(),
            _ => (),
        }
        let _ = self.tx.send(json);
    }

    pub fn print(&self, kind: &str, text: &str, user: Option<&str>) {
        self.emit(&Event::Print {
            kind: kind.into(),
            text: text.into(),
            user: user.map(str::to_string),
            cont: false,
        });
    }

    pub fn echo(&self, line: &str, cont: bool, user: &str) {
        self.emit(&Event::Print {
            kind: "echo".into(),
            text: line.into(),
            user: Some(user.into()),
            cont,
        });
    }

    pub fn prompt(&self, pending: bool) {
        self.pending.store(pending, Ordering::Relaxed);
        self.emit(&Event::Prompt { pending });
    }

    pub fn clear(&self) {
        self.emit(&Event::Clear);
    }
}
