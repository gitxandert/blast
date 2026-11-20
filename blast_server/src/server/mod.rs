//! HTTP and WebSocket front end.
//!
//! Users are identified by the `Tailscale-User-Login` header that
//! `tailscale serve` adds to tailnet requests; a request without it (from
//! loopback) is the user `local`. The server only listens on loopback, so
//! nothing reaches it except through Tailscale or from this machine.

mod files;
mod ws;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Query, State};
use axum::http::{StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::config::{Config, Prefs};
use crate::engine::session::HistKey;
use crate::engine::{Engine, output, pipewire};

/// (login, display name) from `tailscale whois`.
type Who = (String, Option<String>);

#[derive(Clone)]
pub struct App {
    pub cfg: Arc<Config>,
    pub engine: Arc<Engine>,
    pub whois: Arc<Mutex<HashMap<IpAddr, (Who, Instant)>>>,
}

impl App {
    pub fn new(cfg: Arc<Config>, engine: Arc<Engine>) -> App {
        App {
            cfg,
            engine,
            whois: Arc::default(),
        }
    }
}

/// The user a request is from.
pub struct User {
    pub login: String,
    pub name: String,
}

impl User {
    pub fn hist(&self, cfg: &Config) -> HistKey {
        let path = cfg.history_path(&self.login);
        let key = match cfg.engine.history.as_str() {
            "per-user" | "off" => self.login.clone(),
            _ => "shared".to_string(),
        };
        HistKey { key, path }
    }
}

impl FromRequestParts<App> for User {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<User, Response> {
        let header = |k: &str| {
            parts
                .headers
                .get(k)
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        let forbid = |why: String| (StatusCode::FORBIDDEN, why).into_response();
        let (login, ts_name) = match peer {
            // loopback: `tailscale serve` (which sets these headers and
            // strips any a client sent) or someone on this machine
            None => (
                header("tailscale-user-login").unwrap_or_else(|| "local".into()),
                header("tailscale-user-name"),
            ),
            Some(ip) if ip.is_loopback() => (
                header("tailscale-user-login").unwrap_or_else(|| "local".into()),
                header("tailscale-user-name"),
            ),
            // straight from the tailnet: ask tailscaled who it is
            Some(ip) if is_tailnet(ip) => match app.whois(ip).await {
                Some(who) => who,
                None => return Err(forbid(format!("tailscale whois {ip} failed"))),
            },
            Some(ip) => return Err(forbid(format!("{ip} is not on the tailnet"))),
        };
        if !app.cfg.user_allowed(&login) {
            return Err(forbid(format!("{login} is not allowed here")));
        }
        let name = app
            .cfg
            .users
            .get(&login)
            .and_then(|u| u.display_name.clone());
        let name = name
            .or(ts_name)
            .unwrap_or_else(|| app.cfg.display_name(&login));
        Ok(User { login, name })
    }
}

/// Tailscale's address ranges: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn is_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xC0) == 64
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_tailnet(IpAddr::V4(v4));
            }
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

impl App {
    /// (login, display name) of a tailnet address, cached for 10 minutes.
    async fn whois(&self, ip: IpAddr) -> Option<(String, Option<String>)> {
        if let Some((who, at)) = self.whois.lock().unwrap().get(&ip)
            && at.elapsed() < Duration::from_secs(600)
        {
            return Some(who.clone());
        }
        let who = tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new("tailscale")
                .args(["whois", "--json", &ip.to_string()])
                .output()
                .ok()?;
            let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
            let profile = v.get("UserProfile")?;
            let login = profile.get("LoginName")?.as_str()?.to_string();
            let name = profile
                .get("DisplayName")
                .and_then(|n| n.as_str())
                .map(str::to_string);
            Some((login, name))
        })
        .await
        .ok()??;
        self.whois
            .lock()
            .unwrap()
            .insert(ip, (who.clone(), Instant::now()));
        Some(who)
    }
}

/// An error answer: `{"error": "..."}` with a status.
pub struct ApiErr(StatusCode, String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub fn bad(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, msg.into())
}

pub fn not_found(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::NOT_FOUND, msg.into())
}

pub fn router(app: App) -> Router {
    let web_root = app.cfg.server.web_root.clone();
    let max = (app.cfg.server.max_upload_mb as usize).saturating_mul(1024 * 1024);
    Router::new()
        .route("/api/ws", get(ws::handler))
        .route("/api/me", get(me))
        .route("/api/prefs", post(save_prefs).delete(reset_prefs))
        .route("/api/engine", get(engine_info))
        .route("/api/engine/restart", post(restart))
        .route("/api/devices", get(devices))
        .route("/api/output", post(set_output))
        .route("/api/pipewire", get(pw_routing))
        .route("/api/pipewire/route", post(pw_route))
        .route("/api/card", get(card))
        .route("/api/check", post(check))
        .merge(files::routes())
        .fallback(get(move |uri: axum::http::Uri| {
            files::static_file(web_root.clone(), uri)
        }))
        .layer(DefaultBodyLimit::max(max))
        .with_state(app)
}

async fn me(State(app): State<App>, user: User) -> Json<serde_json::Value> {
    let hist = user.hist(&app.cfg);
    Json(json!({
        "login": user.login,
        "name": user.name,
        "prefs": app.cfg.prefs_for(&user.login),
        "history_key": hist.key,
        "stream": { "format": app.cfg.stream.format, "chunk_ms": app.cfg.stream.chunk_ms },
        "pipewire_routing": app.cfg.engine.pipewire_routing,
        "config_path": app.cfg.path.as_ref().map(|p| p.display().to_string()),
    }))
}

async fn save_prefs(
    State(app): State<App>,
    user: User,
    Json(changes): Json<Prefs>,
) -> Result<Json<Prefs>, ApiErr> {
    app.cfg
        .save_prefs(&user.login, &changes)
        .map(Json)
        .map_err(bad)
}

async fn reset_prefs(State(app): State<App>, user: User) -> Json<Prefs> {
    Json(app.cfg.reset_prefs(&user.login))
}

async fn engine_info(State(app): State<App>, _user: User) -> Json<serde_json::Value> {
    Json(json!({
        "info": app.engine.info(),
        "status": app.engine.status(),
        "tracks": app.engine.tracks().await,
    }))
}

#[derive(Deserialize)]
struct RestartReq {
    #[serde(default = "yes")]
    keep_state: bool,
}

fn yes() -> bool {
    true
}

async fn restart(
    State(app): State<App>,
    user: User,
    Json(r): Json<RestartReq>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    app.engine.hub.print(
        "info",
        &format!(
            "{} restarted the engine{}.",
            user.name,
            if r.keep_state {
                " (keeping the session)"
            } else {
                ""
            }
        ),
        None,
    );
    app.engine
        .restart_async(r.keep_state, None)
        .await
        .map_err(bad)?;
    Ok(Json(json!({ "info": app.engine.info() })))
}

#[derive(Deserialize)]
struct DevicesQ {
    #[serde(default)]
    all: bool,
}

async fn devices(
    State(app): State<App>,
    _user: User,
    Query(q): Query<DevicesQ>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let rate = app.engine.info().rate;
    let list = tokio::task::spawn_blocking(move || output::list_devices(rate))
        .await
        .map_err(|e| bad(e.to_string()))?;
    let list: Vec<_> = list
        .into_iter()
        .filter(|d| q.all || d.common || d.is_default)
        .collect();
    Ok(Json(json!({
        "current": app.engine.output.info(),
        "rate": rate,
        "devices": list,
    })))
}

#[derive(Deserialize)]
struct OutputReq {
    id: String,
    /// Restart at the device's rate if it can't run at the session's.
    #[serde(default)]
    restart: bool,
}

async fn set_output(
    State(app): State<App>,
    user: User,
    Json(r): Json<OutputReq>,
) -> Result<Json<output::OutputInfo>, ApiErr> {
    let info = app.engine.set_output(r.id, r.restart).await.map_err(bad)?;
    app.engine.hub.print(
        "info",
        &format!("{} switched the output to {}.", user.name, info.name),
        None,
    );
    Ok(Json(info))
}

async fn pw_routing(State(app): State<App>, _user: User) -> Json<pipewire::Routing> {
    if !app.cfg.engine.pipewire_routing {
        return Json(pipewire::Routing {
            note: Some("pipewire_routing is off in the config".into()),
            ..Default::default()
        });
    }
    Json(
        tokio::task::spawn_blocking(pipewire::routing)
            .await
            .unwrap_or_default(),
    )
}

#[derive(Deserialize)]
struct RouteReq {
    sink: String,
}

async fn pw_route(
    State(app): State<App>,
    user: User,
    Json(r): Json<RouteReq>,
) -> Result<Json<pipewire::Routing>, ApiErr> {
    if !app.cfg.engine.pipewire_routing {
        return Err(bad("pipewire_routing is off in the config"));
    }
    let sink = r.sink.clone();
    tokio::task::spawn_blocking(move || pipewire::route(&sink))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(bad)?;
    // WirePlumber moves the stream asynchronously
    tokio::time::sleep(Duration::from_millis(300)).await;
    let routing = tokio::task::spawn_blocking(pipewire::routing)
        .await
        .unwrap_or_default();
    let desc = routing
        .sinks
        .iter()
        .find(|s| s.name == r.sink)
        .map_or(r.sink.clone(), |s| s.description.clone());
    app.engine.hub.print(
        "info",
        &format!("{} routed the output to {desc}.", user.name),
        None,
    );
    app.engine.hub.emit(&crate::engine::hub::Event::Engine);
    Ok(Json(routing))
}

async fn card(_user: User) -> String {
    blast::audio_processing::commands::lang::card()
}

#[derive(Deserialize)]
struct CheckReq {
    #[serde(default = "editor")]
    name: String,
    text: String,
}

fn editor() -> String {
    "editor".into()
}

async fn check(
    State(app): State<App>,
    _user: User,
    Json(r): Json<CheckReq>,
) -> Json<serde_json::Value> {
    let (errors, warnings) = app.engine.check(r.name, r.text).await;
    Json(json!({ "ok": errors.is_empty(), "errors": errors, "warnings": warnings }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailnet_ranges() {
        let t = |s: &str| is_tailnet(s.parse().unwrap());
        assert!(t("100.97.2.104"));
        assert!(t("100.64.0.1"));
        assert!(t("100.127.255.255"));
        assert!(!t("100.128.0.1"));
        assert!(!t("100.63.0.1"));
        assert!(!t("192.168.1.2"));
        assert!(t("fd7a:115c:a1e0::6f4d:269"));
        assert!(!t("fd7a:115c:a1e1::1"));
        assert!(t("::ffff:100.97.2.104"));
    }
}
