//! Two small HTTP servers:
//! * the browser bridge on 127.0.0.1 (the extension, bookmarklets):
//!     Legacy (bookmarklets): GET /add?url=…&ref=…  GET /batch?urls=…&ref=…  GET /show  GET /ping
//!     Extension (JSON):      GET /api/ping  POST /api/add {urls, referer, cookies, userAgent, fileName, autostart, mode}
//!   /api/* refuses requests from ordinary web pages (they carry an http(s) Origin); legacy routes only
//!   ever open the Add dialog.
//! * the phone remote on the home network: a web page plus a PIN-protected API; 10 wrong PINs lock that
//!   device out for 5 minutes, and the QR pairing page is only served to this PC itself.

use crate::manager::Manager;
use crate::model::*;
use crate::net;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::Router;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Ctx {
    mgr: Arc<Manager>,
    app: AppHandle,
}

fn from_extension(origin: &str) -> bool {
    let o = origin.to_ascii_lowercase();
    o.starts_with("chrome-extension://") || o.starts_with("moz-extension://") || o.starts_with("extension://")
}

fn with_headers(mut r: Response, cors_origin: Option<&str>) -> Response {
    let h = r.headers_mut();
    h.insert("Cache-Control", HeaderValue::from_static("no-store"));
    if let Some(o) = cors_origin.and_then(|o| HeaderValue::from_str(o).ok()) {
        h.insert("Access-Control-Allow-Origin", o);
        h.insert("Access-Control-Allow-Methods", HeaderValue::from_static("GET, POST, OPTIONS"));
        h.insert("Access-Control-Allow-Headers", HeaderValue::from_static("Content-Type"));
        h.insert("Access-Control-Allow-Private-Network", HeaderValue::from_static("true"));
    }
    r
}

// ------------------------------------------------------------------ browser bridge

static LOCAL: Mutex<Option<CancellationToken>> = Mutex::new(None);

/// Starts (or restarts) the bridge on 127.0.0.1:port. False when the port is taken.
/// Binds, retrying for a few seconds: a server just stopped may still be letting go of the port.
async fn bind_retry(host: &str, port: u16) -> Option<tokio::net::TcpListener> {
    for _ in 0..15 {
        if let Ok(l) = tokio::net::TcpListener::bind((host, port)).await {
            return Some(l);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    None
}

pub async fn start_local(mgr: Arc<Manager>, app: AppHandle, port: u16) -> bool {
    stop_local();
    let Some(listener) = bind_retry("127.0.0.1", port).await else { return false };
    let token = CancellationToken::new();
    *LOCAL.lock() = Some(token.clone());
    let router = Router::new().fallback(local_handler).with_state(Ctx { mgr, app });
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, router).with_graceful_shutdown(async move { token.cancelled().await }).await;
    });
    true
}

pub fn stop_local() {
    if let Some(t) = LOCAL.lock().take() {
        t.cancel();
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ExtensionAdd {
    urls: Vec<String>,
    referer: Option<String>,
    cookies: Option<String>,
    user_agent: Option<String>,
    file_name: Option<String>,
    autostart: bool,
    mode: Option<String>,
}

async fn local_handler(State(ctx): State<Ctx>, method: Method, uri: Uri, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let origin = headers.get("origin").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let ext = from_extension(&origin);
    let cors = ext.then_some(origin.as_str());
    let path = uri.path().trim_matches('/').to_ascii_lowercase();

    if method == Method::OPTIONS {
        return with_headers(StatusCode::NO_CONTENT.into_response(), cors);
    }
    let jsonr = |code: StatusCode, v: Value| (code, [("Content-Type", "application/json")], v.to_string()).into_response();

    if let Some(api) = path.strip_prefix("api/") {
        // An Origin that isn't an extension means a web page — refuse. No Origin = a local program.
        if !origin.is_empty() && !ext {
            return with_headers(jsonr(StatusCode::FORBIDDEN, json!({ "ok": false, "error": "forbidden" })), None);
        }
        return match (api, method.as_str()) {
            ("ping", _) => with_headers(jsonr(StatusCode::OK, json!({ "ok": true, "app": "FluxDM", "version": env!("CARGO_PKG_VERSION") })), cors),
            // "FluxDM.exe --exit": a clean quit. Browsers always send an Origin with a POST, so no page can do this.
            ("exit", "POST") if origin.is_empty() => {
                let app = ctx.app.clone();
                tauri::async_runtime::spawn(async move { crate::exit_app(&app).await });
                with_headers(jsonr(StatusCode::OK, json!({ "ok": true })), None)
            }
            ("add", "POST") => {
                let a: ExtensionAdd = serde_json::from_slice(&body).unwrap_or_default();
                // Nothing usable (bad JSON, blob:/data: links…) is an error the extension can react to,
                // not a silent "ok" that makes it drop the browser's own download.
                if handle_extension_add(&ctx, a, ext) {
                    with_headers(jsonr(StatusCode::OK, json!({ "ok": true })), cors)
                } else {
                    with_headers(jsonr(StatusCode::BAD_REQUEST, json!({ "ok": false, "error": "no downloadable link" })), cors)
                }
            }
            _ => with_headers(jsonr(StatusCode::NOT_FOUND, json!({ "ok": false })), cors),
        };
    }

    let referer = q.get("ref").filter(|r| net::is_http_url(r)).map(|r| format!("Referer: {r}")).unwrap_or_default();
    let resp = if path == "ping" {
        "FluxDM".into_response()
    } else {
        Html("<!doctype html><meta charset=utf-8><title>FluxDM</title><body style=\"font:15px system-ui;background:#1f2430;color:#e8eaf0;display:grid;place-items:center;height:90vh\">Sent to FluxDM ✓<script>setTimeout(()=>window.close(),700)</script>").into_response()
    };
    match path.as_str() {
        "add" => {
            if let Some(url) = q.get("url").filter(|u| net::is_download_source(u)) {
                crate::show_add(&ctx.app, crate::AddRequest { urls: url.clone(), headers: referer, ..Default::default() });
            }
        }
        "batch" => {
            if let Some(list) = q.get("urls") {
                crate::show_add(&ctx.app, crate::AddRequest { urls: list.clone(), headers: referer, ..Default::default() });
            }
        }
        "show" => crate::show_main(&ctx.app),
        _ => {}
    }
    with_headers(resp, None)
}

fn handle_extension_add(ctx: &Ctx, a: ExtensionAdd, from_ext: bool) -> bool {
    // From the browser: web links and magnet links (never local file paths).
    let mut urls: Vec<String> = vec![];
    for u in a.urls.iter().map(|u| u.trim().to_string()) {
        if (net::is_http_url(&u) || net::is_magnet(&u)) && !urls.contains(&u) {
            urls.push(u);
        }
    }
    if urls.is_empty() {
        return false;
    }
    let one = |s: &str| s.replace(['\r', '\n'], "").trim().to_string();
    let mut headers = vec![];
    if let Some(r) = a.referer.as_deref().filter(|r| net::is_http_url(r)) {
        headers.push(format!("Referer: {}", one(r)));
    }
    if let Some(c) = a.cookies.as_deref().filter(|c| !c.trim().is_empty()) {
        headers.push(format!("Cookie: {}", one(c)));
    }
    if let Some(ua) = a.user_agent.as_deref().filter(|u| !u.trim().is_empty()) {
        headers.push(format!("User-Agent: {}", one(ua)));
    }
    let h = headers.join("\n");
    let name = if urls.len() == 1 { a.file_name.clone().filter(|n| !n.trim().is_empty()) } else { None };
    let video = a.mode.as_deref() == Some("video");
    let mgr = &ctx.mgr;
    if a.autostart && from_ext {
        let s = mgr.settings();
        let mut items: Vec<Item> = vec![];
        for u in &urls {
            let mut it = if net::is_torrent_source(u) {
                mgr.create_torrent_item(u, None, None, None)
            } else if video {
                mgr.create_video_item(u, None, &s.default_video_format, false, None)
            } else {
                mgr.create_item(u, name.as_deref(), &h, &items)
            };
            it.headers = h.clone();
            items.push(it);
        }
        let first = items[0].file_name.clone();
        let n = items.len();
        mgr.add(items, true);
        if s.notify_on_complete {
            mgr.toast("Download started", &if n == 1 { first } else { format!("{n} downloads from your browser") }, None);
        }
    } else {
        crate::show_add(
            &ctx.app,
            crate::AddRequest {
                urls: urls.join("\n"),
                headers: h,
                name: if video { None } else { name },
                video,
                ..Default::default()
            },
        );
    }
    true
}

// ------------------------------------------------------------------ phone remote

static REMOTE: Mutex<Option<CancellationToken>> = Mutex::new(None);
static FAILURES: Mutex<Option<HashMap<String, (u32, Option<Instant>)>>> = Mutex::new(None);

pub async fn start_remote(mgr: Arc<Manager>, app: AppHandle, port: u16) -> bool {
    stop_remote();
    let Some(listener) = bind_retry("0.0.0.0", port).await else { return false };
    let token = CancellationToken::new();
    *REMOTE.lock() = Some(token.clone());
    let router = Router::new().fallback(remote_handler).with_state(Ctx { mgr, app });
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async move { token.cancelled().await })
            .await;
    });
    true
}

pub fn stop_remote() {
    if let Some(t) = REMOTE.lock().take() {
        t.cancel();
    }
}

pub fn remote_running() -> bool {
    REMOTE.lock().is_some()
}

pub fn pairing_urls(port: i32) -> Vec<String> {
    crate::win::lan_addresses().into_iter().map(|ip| format!("http://{ip}:{port}/")).collect()
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn remote_handler(State(ctx): State<Ctx>, ConnectInfo(peer): ConnectInfo<SocketAddr>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let path = uri.path().trim_matches('/').to_ascii_lowercase();
    let jsonr = |code: StatusCode, v: Value| (code, [("Content-Type", "application/json"), ("Cache-Control", "no-store")], v.to_string()).into_response();
    if path.is_empty() || path == "index.html" {
        return Html(include_str!("remote.html")).into_response();
    }
    let s = ctx.mgr.settings();
    if path == "qr" {
        if !peer.ip().is_loopback() {
            return (StatusCode::FORBIDDEN, "forbidden").into_response();
        }
        return Html(pairing_html(&pairing_urls(s.remote_port), &s.remote_pin)).into_response();
    }
    let Some(api) = path.strip_prefix("api/") else { return (StatusCode::NOT_FOUND, "not found").into_response() };

    // --- authentication with lockout
    let who = peer.ip().to_string();
    {
        let mut f = FAILURES.lock();
        let map = f.get_or_insert_with(HashMap::new);
        if let Some((_, Some(until))) = map.get(&who) {
            if *until > Instant::now() {
                return jsonr(StatusCode::TOO_MANY_REQUESTS, json!({ "error": "locked" }));
            }
        }
        let pin = headers.get("x-pin").and_then(|v| v.to_str().ok()).unwrap_or("");
        if s.remote_pin.is_empty() || !ct_eq(pin, &s.remote_pin) {
            let e = map.entry(who.clone()).or_insert((0, None));
            e.0 += 1;
            if e.0 >= 10 {
                *e = (0, Some(Instant::now() + Duration::from_secs(300)));
            }
            return jsonr(StatusCode::UNAUTHORIZED, json!({ "error": "pin" }));
        }
        map.remove(&who);
    }

    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let str_of = |k: &str| b[k].as_str().unwrap_or("").to_string();
    let mgr = ctx.mgr.clone();
    let post = method == Method::POST;
    match (api, post) {
        ("state", _) => jsonr(StatusCode::OK, remote_state(&mgr)),
        ("item", true) => {
            let id = str_of("id");
            if mgr.item(&id).is_none() {
                return jsonr(StatusCode::NOT_FOUND, json!({ "error": "not found" }));
            }
            match str_of("action").as_str() {
                "pause" => {
                    let m = mgr.clone();
                    tauri::async_runtime::spawn(async move { m.pause(&id).await });
                }
                "resume" => mgr.start(&id),
                "remove" => {
                    let m = mgr.clone();
                    tauri::async_runtime::spawn(async move { m.remove(&id, false).await });
                }
                "limit" => {
                    let k = b["kbps"].as_i64().unwrap_or(0).max(0) as i32;
                    mgr.with_item(&id, |it| it.speed_limit_kbps = k);
                    mgr.mark_dirty();
                }
                _ => return jsonr(StatusCode::BAD_REQUEST, json!({ "error": "unknown action" })),
            }
            jsonr(StatusCode::OK, json!({ "ok": true }))
        }
        ("all", true) => {
            if str_of("action") == "pause" {
                let m = mgr.clone();
                tauri::async_runtime::spawn(async move { m.pause_all().await });
            } else {
                let ids: Vec<String> = mgr.st.lock().items.iter().filter(|i| matches!(i.status, DlStatus::Paused | DlStatus::Error)).map(|i| i.id.clone()).collect();
                for id in ids {
                    mgr.start(&id);
                }
            }
            jsonr(StatusCode::OK, json!({ "ok": true }))
        }
        ("add", true) => {
            let url = str_of("url").trim().to_string();
            // Magnet/.torrent links are unambiguous, so they're recognised regardless of the "video" box.
            if net::is_magnet(&url) || (net::is_http_url(&url) && net::is_torrent_source(&url)) {
                let it = mgr.create_torrent_item(&url, None, None, None);
                mgr.add(vec![it], true);
                return jsonr(StatusCode::OK, json!({ "ok": true }));
            }
            if !net::is_http_url(&url) {
                return jsonr(StatusCode::BAD_REQUEST, json!({ "error": "not a link" }));
            }
            let it = if b["video"].as_bool() == Some(true) {
                mgr.create_video_item(&url, None, &s.default_video_format, false, None)
            } else {
                mgr.create_item(&url, None, "", &[])
            };
            mgr.add(vec![it], true);
            jsonr(StatusCode::OK, json!({ "ok": true }))
        }
        ("limit", true) => {
            let k = b["kbps"].as_i64().unwrap_or(0).max(0) as i32;
            mgr.st.lock().settings.global_speed_limit_kbps = k;
            mgr.mark_dirty();
            let _ = tauri::Emitter::emit(&ctx.app, "settings-changed", json!({}));
            jsonr(StatusCode::OK, json!({ "ok": true }))
        }
        _ => jsonr(StatusCode::NOT_FOUND, json!({ "error": "not found" })),
    }
}

fn remote_state(mgr: &Manager) -> Value {
    let speed = *mgr.total_speed.lock();
    let running = mgr.running_count();
    let st = mgr.st.lock();
    let s = &st.settings;
    let items: Vec<Value> = st
        .items
        .iter()
        .take(100)
        .map(|i| {
            let downloading = i.status == DlStatus::Downloading;
            json!({
                "id": i.id,
                "name": i.file_name,
                "status": i.status.name(),
                "statusText": i.status_text(),
                "progress": i.progress(),
                "progressText": if i.status == DlStatus::Completed || i.total_size > 0 { format!("{:.1}%", i.progress()) } else if i.downloaded > 0 { fmt::size(i.downloaded) } else { String::new() },
                "sizeText": if i.total_size > 0 { fmt::size(i.total_size) } else { "?".into() },
                "speedText": if downloading { fmt::speed(i.speed) } else { String::new() },
                "eta": if downloading && i.speed > 1.0 && i.total_size > 0 { fmt::eta((i.total_size - i.downloaded) as f64 / i.speed) } else { String::new() },
                "video": i.is_video,
                "torrent": i.is_torrent,
                "limitKBps": i.speed_limit_kbps,
                "limitText": if i.speed_limit_kbps > 0 { fmt::limit(i.speed_limit_kbps) } else { String::new() },
            })
        })
        .collect();
    json!({
        "speed": speed,
        "speedText": fmt::speed(speed),
        "running": running,
        "limitKBps": s.global_speed_limit_kbps,
        "limitText": fmt::limit(Manager::effective_global_kbps(s)),
        "schedule": Manager::schedule_active(s),
        "items": items,
    })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The pairing page: a QR code (drawn here, works offline) with the address and PIN.
fn pairing_html(urls: &[String], pin: &str) -> String {
    let first = urls.first().cloned().unwrap_or_default();
    let qr = qrcode::QrCode::new(format!("{first}#pin={pin}").as_bytes())
        .map(|c| c.render::<qrcode::render::svg::Color>().min_dimensions(220, 220).quiet_zone(false).build())
        .unwrap_or_default();
    let others: String = urls.iter().skip(1).map(|u| format!("<li>{}</li>", esc(u))).collect();
    let others = if others.is_empty() { String::new() } else { format!("<p>Other addresses of this PC, if the first doesn't work:</p><ul>{others}</ul>") };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>FluxDM phone remote</title>
<style>
body{{font:15px system-ui,"Segoe UI",sans-serif;background:#16181d;color:#e8eaee;display:grid;place-items:center;min-height:95vh;margin:0}}
.box{{background:#22252c;border-radius:16px;padding:28px 34px;text-align:center;max-width:460px}}
#qr{{background:#fff;padding:14px;border-radius:12px;display:inline-block;margin:16px 0}}
.pin{{font-size:32px;letter-spacing:8px;font-weight:700}}
code{{background:#30343c;padding:3px 8px;border-radius:6px}}
p,li{{color:#b8bdc6}} ul{{text-align:left}}
</style></head><body><div class="box">
<h2>Open FluxDM on your phone</h2>
<p>Connect your phone to the same Wi-Fi, then scan this code with the camera.</p>
<div id="qr">{qr}</div>
<p>Or type <code>{addr}</code> and enter the PIN:</p>
<div class="pin">{pin}</div>
{others}
<p style="font-size:13px">If the phone can't connect, allow FluxDM through Windows Firewall on <b>private</b> networks (Settings → Phone remote).</p>
</div></body></html>"#,
        addr = esc(&first),
        pin = esc(pin),
    )
}
