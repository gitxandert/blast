//! The REPL socket. Browser → server (JSON, tagged by `t`):
//!
//! - `{"t":"submit","line":"…"}`: one line at the prompt (a pasted block is
//!   sent line by line, as a terminal would)
//! - `{"t":"cancel"}`: Ctrl-C (drops an open block)
//! - `{"t":"complete","id":N,"line":"…","cursor":BYTE}` → `completions`
//! - `{"t":"history"}` → `history` with `replace: true`
//! - `{"t":"listen","on":BOOL}`: start or stop the audio stream
//!
//! Server → browser: `hello`, `scrollback`, `completions`, `listen`, every
//! hub event (`print`, `prompt`, `clear`, `history`, `engine`, `status`),
//! and binary audio messages (see `engine::stream`) while listening.

use axum::extract::State;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use super::{App, User};

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum In {
    Submit {
        line: String,
    },
    Cancel,
    Complete {
        id: u64,
        line: String,
        cursor: usize,
    },
    History,
    Listen {
        on: bool,
    },
    Ping,
}

pub async fn handler(State(app): State<App>, user: User, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| session(app, user, socket))
}

fn scrollback_msg(app: &App) -> String {
    let events = app.engine.hub.scrollback();
    let mut s = String::from("{\"t\":\"scrollback\",\"events\":[");
    for (i, e) in events.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(e);
    }
    s.push_str("]}");
    s
}

async fn session(app: App, user: User, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let mut events = app.engine.hub.subscribe();
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(64);
    let hist = user.hist(&app.cfg);

    let hello = json!({
        "t": "hello",
        "login": user.login,
        "name": user.name,
        "pending": app.engine.hub.pending(),
        "history_key": hist.key,
    });
    let history = app.engine.history(hist.clone()).await;
    let first = [
        hello.to_string(),
        scrollback_msg(&app),
        json!({"t": "history", "key": hist.key, "entries": history, "replace": true}).to_string(),
        json!({"t": "status", "status": app.engine.status()}).to_string(),
    ];
    for m in first {
        if sink.send(Message::Text(m.into())).await.is_err() {
            return;
        }
    }

    // the writer: everything to the browser goes through `out_tx`
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let mut audio: Option<tokio::sync::broadcast::Receiver<axum::body::Bytes>> = None;
    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(req) = serde_json::from_str::<In>(&text) else {
                    let _ = out_tx.send(text_msg(json!({"t":"print","kind":"err","text":"Err: bad message from the browser"}))).await;
                    continue;
                };
                match req {
                    In::Submit { line } => {
                        // a line is one line: a stray newline would be a new statement
                        let line = line.replace(['\r', '\n'], " ");
                        app.engine.submit(line, user.name.clone(), hist.clone());
                    }
                    In::Cancel => app.engine.cancel(),
                    In::Complete { id, line, cursor } => {
                        let list = app.engine.complete(line, cursor).await;
                        let _ = out_tx.send(text_msg(json!({"t":"completions","id":id,"list":list}))).await;
                    }
                    In::History => {
                        let entries = app.engine.history(hist.clone()).await;
                        let _ = out_tx.send(text_msg(json!({"t":"history","key":hist.key,"entries":entries,"replace":true}))).await;
                    }
                    In::Listen { on } => {
                        let max = app.cfg.stream.max_listeners;
                        let ok = match (on, audio.is_some()) {
                            (true, false) if app.engine.listeners.load(std::sync::atomic::Ordering::Relaxed) >= max => false,
                            (true, false) => {
                                audio = Some(app.engine.audio.subscribe());
                                app.engine.listen(true);
                                true
                            }
                            (false, true) => {
                                audio = None;
                                app.engine.listen(false);
                                true
                            }
                            _ => true,
                        };
                        let reply = if ok {
                            json!({"t":"listen","on":audio.is_some()})
                        } else {
                            json!({"t":"listen","on":false,"error":format!("{max} browsers are already listening")})
                        };
                        let _ = out_tx.send(text_msg(reply)).await;
                    }
                    In::Ping => {
                        let _ = out_tx.send(text_msg(json!({"t":"pong"}))).await;
                    }
                }
            }
            ev = events.recv() => {
                match ev {
                    Ok(json) => {
                        if out_tx.send(Message::Text(Utf8Bytes::from(json.to_string()))).await.is_err() {
                            break;
                        }
                    }
                    // fell behind: start over from the scrollback
                    Err(RecvError::Lagged(_)) => {
                        let _ = out_tx.send(Message::Text(scrollback_msg(&app).into())).await;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            frame = async {
                match audio.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match frame {
                    Ok(bytes) => {
                        // never queue audio behind a slow socket: drop it
                        let _ = out_tx.try_send(Message::Binary(bytes));
                    }
                    Err(RecvError::Lagged(_)) => (),
                    Err(RecvError::Closed) => audio = None,
                }
            }
        }
    }
    if audio.is_some() {
        app.engine.listen(false);
    }
    drop(out_tx);
    let _ = writer.await;
}

fn text_msg(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}
