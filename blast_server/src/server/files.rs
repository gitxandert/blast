//! Sources (`.blast` scripts) and assets (audio files), plus the static
//! app. Every path from a browser is relative, checked component by
//! component, and must stay inside its folder.

use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use blast::file_parsing::{decode_file, is_supported_extension, scan_assets};
use serde::Deserialize;
use serde_json::json;

use super::{ApiErr, App, User, bad, not_found};

pub fn routes() -> axum::Router<App> {
    axum::Router::new()
        .route("/api/sources", get(list_sources))
        .route("/api/sources/{*name}", get(read_source).put(write_source))
        .route("/api/assets", get(list_assets))
        .route("/api/assets/raw", get(asset_raw))
        .route("/api/assets/preview", get(asset_preview))
        .route("/api/assets/upload", axum::routing::put(upload_asset))
}

/// `rel` joined to `root`, if every component is a plain name (no `..`,
/// no root, no leading `.`).
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel.trim_start_matches('/'));
    let mut out = root.to_path_buf();
    let mut any = false;
    for c in rel.components() {
        match c {
            Component::Normal(p) => {
                let s = p.to_str()?;
                if s.starts_with('.') || s.contains('\\') {
                    return None;
                }
                out.push(p);
                any = true;
            }
            _ => return None,
        }
    }
    any.then_some(out)
}

fn modified(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

// ---- sources ----

fn walk(
    dir: &Path,
    root: &Path,
    ext: Option<&str>,
    out: &mut Vec<(String, std::fs::Metadata)>,
    depth: usize,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if depth < 8 {
                walk(&path, root, ext, out, depth + 1);
            }
        } else if ext.is_none_or(|x| path.extension().and_then(|e| e.to_str()) == Some(x))
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push((rel.to_string_lossy().into_owned(), meta));
        }
    }
}

async fn list_sources(State(app): State<App>, _u: User) -> Json<serde_json::Value> {
    let dir = app.cfg.engine.sources_dir.clone();
    let mut found = Vec::new();
    walk(&dir, &dir, Some("blast"), &mut found, 0);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let list: Vec<_> = found
        .into_iter()
        .map(|(rel, meta)| {
            // what `source NAME` takes: no extension
            let name = rel.strip_suffix(".blast").unwrap_or(&rel).to_string();
            json!({ "name": name, "size": meta.len(), "modified": modified(&meta) })
        })
        .collect();
    Json(json!({ "dir": dir.display().to_string(), "sources": list }))
}

fn source_path(app: &App, name: &str) -> Result<PathBuf, ApiErr> {
    let name = name.strip_suffix(".blast").unwrap_or(name);
    if name.is_empty()
        || name
            .chars()
            .any(|c| !(c.is_alphanumeric() || "-_./".contains(c)))
    {
        return Err(bad(
            "source names are letters, digits, '-', '_', '.' and '/'",
        ));
    }
    safe_join(&app.cfg.engine.sources_dir, &format!("{name}.blast"))
        .ok_or_else(|| bad("bad source name"))
}

async fn read_source(
    State(app): State<App>,
    _u: User,
    UrlPath(name): UrlPath<String>,
) -> Result<Response, ApiErr> {
    let path = source_path(&app, &name)?;
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| not_found(format!("{}: {e}", path.display())))?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

#[derive(Deserialize)]
struct WriteQ {
    #[serde(default)]
    overwrite: bool,
}

async fn write_source(
    State(app): State<App>,
    user: User,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<WriteQ>,
    body: String,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let path = source_path(&app, &name)?;
    if !q.overwrite && path.exists() {
        return Err(ApiErr(
            StatusCode::CONFLICT,
            format!("{} exists", path.display()),
        ));
    }
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| bad(e.to_string()))?;
    }
    tokio::fs::write(&path, body)
        .await
        .map_err(|e| bad(e.to_string()))?;
    app.engine.hub.print(
        "info",
        &format!("{} saved {}.", user.name, path.display()),
        None,
    );
    Ok(Json(json!({ "saved": path.display().to_string() })))
}

// ---- assets ----

async fn list_assets(State(app): State<App>, _u: User) -> Result<Json<serde_json::Value>, ApiErr> {
    let dir = app.cfg.engine.assets_dir.clone();
    let d = dir.clone();
    let (playable, all) = tokio::task::spawn_blocking(move || {
        let playable = scan_assets(&d).unwrap_or_default();
        let mut all = Vec::new();
        walk(&d, &d, None, &mut all, 0);
        (playable, all)
    })
    .await
    .map_err(|e| bad(e.to_string()))?;
    let loaded = app.engine.tracks().await;
    let mut list = Vec::new();
    for (rel, meta) in all {
        let full = dir.join(&rel);
        let found = playable.iter().find(|a| a.path == full);
        let ext = Path::new(&rel)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if found.is_none() && !is_supported_extension(&ext) {
            continue;
        }
        list.push(json!({
            "path": rel,
            "size": meta.len(),
            "modified": modified(&meta),
            "track": found.map(|a| a.name.clone()),
            "rate": found.map(|a| a.info.sample_rate),
            "channels": found.map(|a| a.info.num_channels),
            "loaded": found.is_some_and(|a| loaded.contains(&a.name)),
            "playable": found.is_some(),
        }));
    }
    list.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(Json(
        json!({ "dir": dir.display().to_string(), "assets": list }),
    ))
}

#[derive(Deserialize)]
struct AssetQ {
    path: String,
    #[serde(default)]
    overwrite: bool,
}

fn asset_path(app: &App, rel: &str) -> Result<PathBuf, ApiErr> {
    safe_join(&app.cfg.engine.assets_dir, rel).ok_or_else(|| bad("bad asset path"))
}

fn mime(ext: &str) -> &'static str {
    match ext {
        "wav" | "wave" => "audio/wav",
        "aif" | "aiff" | "aifc" => "audio/aiff",
        "flac" => "audio/flac",
        "mp3" | "mp2" | "mp1" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "aac" | "adts" => "audio/aac",
        "au" | "snd" => "audio/basic",
        _ => "application/octet-stream",
    }
}

async fn asset_raw(
    State(app): State<App>,
    _u: User,
    headers: HeaderMap,
    Query(q): Query<AssetQ>,
) -> Result<Response, ApiErr> {
    let path = asset_path(&app, &q.path)?;
    raw_response(&path, &headers).await
}

async fn raw_response(path: &Path, headers: &HeaderMap) -> Result<Response, ApiErr> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| not_found(e.to_string()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    Ok(ranged(
        bytes,
        HeaderValue::from_static(mime(&ext)),
        headers,
        &[],
    ))
}

/// The single byte range a `Range` header asks for, against a body of `len`
/// bytes. `None`: no usable range (absent, not `bytes=`, malformed), so the
/// whole body is served. `Some(None)`: unsatisfiable. `Some(Some((a, b)))`:
/// inclusive bounds. A multi-range request serves its first range (RFC 9110).
fn parse_range(headers: &HeaderMap, len: usize) -> Option<Option<(usize, usize)>> {
    let v = headers.get(header::RANGE)?.to_str().ok()?;
    let specs = v.trim().strip_prefix("bytes=")?;
    // digits only; a number too big for usize saturates
    let num = |s: &str| -> Option<usize> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(s.parse::<usize>().unwrap_or(usize::MAX))
    };
    let mut found = None;
    for spec in specs.split(',') {
        let (a, b) = spec.trim().split_once('-')?;
        let (a, b) = (a.trim(), b.trim());
        let range = if a.is_empty() {
            let n = num(b)?;
            (n != 0 && len != 0).then(|| (len.saturating_sub(n), len - 1))
        } else {
            let a = num(a)?;
            let b = if b.is_empty() { usize::MAX } else { num(b)? };
            if b < a {
                return None; // invalid range: ignore the header
            }
            (a < len).then(|| (a, b.min(len - 1)))
        };
        if found.is_none() {
            found = range;
        }
    }
    Some(found)
}

/// `body` as a response that honours `Range` (iOS Safari plays audio only
/// from servers that answer it with 206). `extra` headers are added to every
/// reply.
fn ranged(
    body: Vec<u8>,
    content_type: HeaderValue,
    headers: &HeaderMap,
    extra: &[(HeaderName, HeaderValue)],
) -> Response {
    let len = body.len();
    let mut res = match parse_range(headers, len) {
        None => (StatusCode::OK, body).into_response(),
        Some(None) => {
            let mut r = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            r
        }
        Some(Some((a, b))) => {
            let mut r = (StatusCode::PARTIAL_CONTENT, body[a..=b].to_vec()).into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {a}-{b}/{len}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            r
        }
    };
    let h = res.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if res.status() != StatusCode::RANGE_NOT_SATISFIABLE {
        res.headers_mut().insert(header::CONTENT_TYPE, content_type);
    }
    for (k, v) in extra {
        res.headers_mut().insert(k.clone(), v.clone());
    }
    res
}

/// The file decoded by blast's own decoders and served as 16-bit WAV, so
/// every format blast plays (raw PCM, ADPCM, AU…) plays in any browser.
async fn asset_preview(
    State(app): State<App>,
    _u: User,
    headers: HeaderMap,
    Query(q): Query<AssetQ>,
) -> Result<Response, ApiErr> {
    let path = asset_path(&app, &q.path)?;
    preview_response(path, &headers).await
}

async fn preview_response(path: PathBuf, headers: &HeaderMap) -> Result<Response, ApiErr> {
    let file = tokio::task::spawn_blocking(move || decode_file(&path))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(|e| bad(e.to_string()))?;
    let wav = wav16(&file.samples, file.num_channels.max(1), file.sample_rate);
    Ok(ranged(
        wav,
        HeaderValue::from_static("audio/wav"),
        headers,
        &[(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=60"),
        )],
    ))
}

/// Interleaved f32 as a 16-bit WAV, at most 2 channels (the first two).
pub fn wav16(samples: &[f32], channels: u32, rate: u32) -> Vec<u8> {
    let out_ch = channels.min(2);
    let frames = samples.len() / channels as usize;
    let data_len = (frames * out_ch as usize * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&(out_ch as u16).to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * out_ch * 2).to_le_bytes());
    v.extend_from_slice(&((out_ch * 2) as u16).to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for f in samples.chunks_exact(channels as usize) {
        for &x in &f[..out_ch as usize] {
            let s = (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            v.extend_from_slice(&s.to_le_bytes());
        }
    }
    v
}

async fn upload_asset(
    State(app): State<App>,
    user: User,
    Query(q): Query<AssetQ>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let path = asset_path(&app, &q.path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !is_supported_extension(&ext) {
        return Err(bad(format!("'.{ext}' isn't a format blast reads")));
    }
    if !q.overwrite && path.exists() {
        return Err(ApiErr(StatusCode::CONFLICT, format!("{} exists", q.path)));
    }
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| bad(e.to_string()))?;
    }
    // written beside, then renamed: a half-written file is never scanned
    let tmp = path.with_file_name(format!(
        ".upload-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("x")
    ));
    tokio::fs::write(&tmp, &body)
        .await
        .map_err(|e| bad(e.to_string()))?;
    let check = tmp.clone();
    let named = path.clone();
    let probe = tokio::task::spawn_blocking(move || {
        // decode under the real name (the extension picks the decoder)
        let bytes = std::fs::read(&check)?;
        let name = named
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        blast::file_parsing::decode_bytes(&name, bytes).map(|_| ())
    })
    .await
    .map_err(|e| bad(e.to_string()))?;
    if let Err(e) = probe {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(bad(format!("blast can't read that file: {e}")));
    }
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| bad(e.to_string()))?;
    app.engine.hub.print(
        "info",
        &format!(
            "{} uploaded {} ({} KB).",
            user.name,
            q.path,
            body.len() / 1024
        ),
        None,
    );
    Ok(Json(json!({ "saved": q.path })))
}

// ---- the app ----

/// The browser app, built into the binary: (path, contents).
const BUILT_IN: &[(&str, &[u8])] = &[
    ("index.html", include_bytes!("../../web/index.html")),
    ("app.js", include_bytes!("../../web/app.js")),
    ("audio.js", include_bytes!("../../web/audio.js")),
    ("ring.js", include_bytes!("../../web/ring.js")),
    ("worklet.js", include_bytes!("../../web/worklet.js")),
    ("style.css", include_bytes!("../../web/style.css")),
    ("icon.svg", include_bytes!("../../web/icon.svg")),
    (
        "manifest.webmanifest",
        include_bytes!("../../web/manifest.webmanifest"),
    ),
];

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map_or("", |(_, e)| e) {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// Serves the browser app: from `root` when it is set (to work on the app
/// without rebuilding), else the copy built into the binary.
pub async fn static_file(root: PathBuf, uri: Uri) -> Response {
    let rel = uri.path().trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let (name, bytes) = if root.as_os_str().is_empty() {
        match BUILT_IN.iter().find(|(p, _)| *p == rel) {
            Some((p, b)) => (p.to_string(), b.to_vec()),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    } else {
        let Some(path) = safe_join(&root, rel) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let path = if path.is_dir() {
            path.join("index.html")
        } else {
            path
        };
        match tokio::fs::read(&path).await {
            Ok(b) => (path.display().to_string(), b),
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, content_type(&name))
        // the app changes with the binary: always revalidate
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every file in web/ is built in, so the app never 404s on a part.
    #[test]
    fn built_in_has_every_web_file() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(
                BUILT_IN.iter().any(|(p, _)| *p == name),
                "web/{name} is not in BUILT_IN"
            );
        }
        assert_eq!(content_type("app.js"), "text/javascript; charset=utf-8");
    }

    #[test]
    fn safe_join_refuses_escapes() {
        let root = Path::new("/r");
        assert_eq!(
            safe_join(root, "a/b.wav"),
            Some(PathBuf::from("/r/a/b.wav"))
        );
        assert_eq!(safe_join(root, "/a.wav"), Some(PathBuf::from("/r/a.wav")));
        assert_eq!(safe_join(root, "../x"), None);
        assert_eq!(safe_join(root, "a/../../x"), None);
        assert_eq!(safe_join(root, ".hidden"), None);
        assert_eq!(safe_join(root, ""), None);
        assert_eq!(safe_join(root, "a\\..\\b"), None);
    }

    #[test]
    fn wav_header() {
        let w = wav16(&[0.0, 0.5, -0.5, 1.0], 2, 48_000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 48_000);
    }

    fn get(range: Option<&str>, len: usize) -> Response {
        let mut h = HeaderMap::new();
        if let Some(r) = range {
            h.insert(header::RANGE, HeaderValue::from_str(r).unwrap());
        }
        let body: Vec<u8> = (0..len).map(|i| i as u8).collect();
        ranged(body, HeaderValue::from_static("audio/wav"), &h, &[])
    }

    async fn body_of(r: Response) -> Vec<u8> {
        axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    fn hdr(r: &Response, k: HeaderName) -> Option<&str> {
        r.headers().get(k).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn range_full_when_absent() {
        let r = get(None, 300);
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(hdr(&r, header::ACCEPT_RANGES), Some("bytes"));
        assert_eq!(hdr(&r, header::CONTENT_RANGE), None);
        assert_eq!(hdr(&r, header::CONTENT_TYPE), Some("audio/wav"));
        assert_eq!(body_of(r).await.len(), 300);
    }

    #[tokio::test]
    async fn range_slice() {
        let r = get(Some("bytes=0-99"), 300);
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 0-99/300"));
        assert_eq!(hdr(&r, header::ACCEPT_RANGES), Some("bytes"));
        assert_eq!(hdr(&r, header::CONTENT_TYPE), Some("audio/wav"));
        assert_eq!(body_of(r).await.len(), 100);
    }

    #[tokio::test]
    async fn range_open_end() {
        let r = get(Some("bytes=100-"), 300);
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 100-299/300"));
        let b = body_of(r).await;
        assert_eq!(b.len(), 200);
        assert_eq!(b[0], 100);
    }

    #[tokio::test]
    async fn range_suffix() {
        let r = get(Some("bytes=-10"), 300);
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 290-299/300"));
        let b = body_of(r).await;
        assert_eq!(b.len(), 10);
        assert_eq!(b[0], 290u32 as u8);
    }

    #[tokio::test]
    async fn range_past_end_416() {
        let r = get(Some("bytes=300-"), 300);
        assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes */300"));
        assert_eq!(hdr(&r, header::CONTENT_TYPE), None);
        assert!(body_of(r).await.is_empty());
        let r = get(Some("bytes=0-10"), 0);
        assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes */0"));
    }

    #[tokio::test]
    async fn range_end_clamped() {
        let r = get(Some("bytes=0-999999"), 300);
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 0-299/300"));
        assert_eq!(body_of(r).await.len(), 300);
    }

    #[tokio::test]
    async fn range_malformed_ignored() {
        for bad in [
            "bytes=abc-",
            "bytes=1-x",
            "items=0-5",
            "bytes=",
            "bytes=5-2",
            "junk",
        ] {
            let r = get(Some(bad), 300);
            assert_eq!(r.status(), StatusCode::OK, "{bad}");
            assert_eq!(hdr(&r, header::CONTENT_RANGE), None, "{bad}");
            assert_eq!(body_of(r).await.len(), 300, "{bad}");
        }
    }

    #[tokio::test]
    async fn range_multi_first_satisfiable() {
        let r = get(Some("bytes=400-500,0-9"), 300);
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 0-9/300"));
        assert_eq!(body_of(r).await.len(), 10);
        let r = get(Some("bytes=0-9,20-29"), 300);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 0-9/300"));
        let r = get(Some("bytes=400-500,600-"), 300);
        assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    }

    #[tokio::test]
    async fn range_huge_numbers() {
        let big = "99999999999999999999999999";
        let r = get(Some(&format!("bytes=5-{big}")), 300);
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 5-299/300"));
        let r = get(Some(&format!("bytes=-{big}")), 300);
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 0-299/300"));
        let r = get(Some(&format!("bytes={big}-")), 300);
        assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes */300"));
    }

    fn temp_wav(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blast_srvr_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, wav16(&[0.0, 0.25, -0.25, 0.5], 1, 8_000)).unwrap();
        p
    }

    fn range_headers(r: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, HeaderValue::from_str(r).unwrap());
        h
    }

    #[tokio::test]
    async fn raw_handler_serves_206_with_mime() {
        let p = temp_wav("raw.wav");
        let r = raw_response(&p, &range_headers("bytes=0-1"))
            .await
            .ok()
            .unwrap();
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(hdr(&r, header::CONTENT_TYPE), Some("audio/wav"));
        assert_eq!(hdr(&r, header::CONTENT_RANGE), Some("bytes 0-1/52"));
        assert_eq!(body_of(r).await, b"RI");
        let r = raw_response(&p, &HeaderMap::new()).await.ok().unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(hdr(&r, header::ACCEPT_RANGES), Some("bytes"));
    }

    #[tokio::test]
    async fn preview_handler_keeps_cache_control() {
        let p = temp_wav("prev.wav");
        for (h, st) in [
            (HeaderMap::new(), StatusCode::OK),
            (range_headers("bytes=0-3"), StatusCode::PARTIAL_CONTENT),
        ] {
            let r = preview_response(p.clone(), &h).await.ok().unwrap();
            assert_eq!(r.status(), st);
            assert_eq!(hdr(&r, header::CACHE_CONTROL), Some("private, max-age=60"));
            assert_eq!(hdr(&r, header::CONTENT_TYPE), Some("audio/wav"));
            assert_eq!(hdr(&r, header::ACCEPT_RANGES), Some("bytes"));
        }
    }
}
