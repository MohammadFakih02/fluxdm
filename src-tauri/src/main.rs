#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![recursion_limit = "256"]

mod api;
mod covers;
mod ext;
mod http_dl;
mod manager;
mod model;
mod net;
mod store;
mod torrent;
mod update;
mod util;
mod video;
mod webview_guard;
mod win;

use manager::Manager;
use model::*;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager as _, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

type R<T> = Result<T, String>;
type M<'a> = State<'a, Arc<Manager>>;

/// What the Add window opens with.
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct AddRequest {
    pub urls: String,
    pub headers: String,
    pub name: Option<String>,
    pub video: bool,
    /// Pre-selected queue (the one open in the sidebar).
    pub queue: Option<String>,
}

static PENDING_ADD: std::sync::LazyLock<Mutex<std::collections::HashMap<String, AddRequest>>> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
/// Our own last copy, so the clipboard watcher doesn't catch it.
static LAST_CLIP: Mutex<String> = Mutex::new(String::new());
static TRAY_HINT_SHOWN: AtomicBool = AtomicBool::new(false);
static FIREWALL_OFFERED: AtomicBool = AtomicBool::new(false);
/// Toasts are handed to the notification page, which decides what is on screen and when (one at a time, each
/// read in full). Until that page has loaded they wait here; `toast_ready` hands them over.
/// `.0` = page ready, `.1` = waiting toasts.
static TOAST_INBOX: Mutex<(bool, Vec<Value>)> = Mutex::new((false, Vec::new()));
static TOAST_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The toast the page says is on screen (0 = none). Only a hide for that one counts, so the window and the
/// page can never disagree about what is showing.
static TOAST_SHOWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Longest a toast may stay up (the page shows each for 6.5 s, longer while the mouse is on it). If the page
/// goes quiet past this, Rust hides the window and tells the page to start over.
const TOAST_BACKSTOP: Duration = Duration::from_secs(25);

const REMOTE_RULE: &str = "FluxDM phone remote";
const TORRENT_RULE: &str = "FluxDM torrent peers";

// ------------------------------------------------------------------ windows

fn window_theme(value: &str) -> Option<tauri::Theme> {
    match value.to_ascii_lowercase().as_str() {
        "light" => Some(tauri::Theme::Light),
        "dark" => Some(tauri::Theme::Dark),
        _ => None,
    }
}

fn apply_window_theme(app: &AppHandle, value: &str) {
    let theme = window_theme(value);
    for window in app.webview_windows().values() {
        let _ = window.set_theme(theme);
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let was_hidden = !w.is_visible().unwrap_or(true);
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        // WebView2 sometimes paints nothing (white/black) when a window created hidden is first shown.
        if was_hidden {
            webview_guard::touch();
            webview_guard::repaint_nudge(&w);
        }
    }
}

pub fn show_add(app: &AppHandle, req: AddRequest) {
    let (label, created) = pending_add(req);
    if !created {
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.emit("add-links", ());
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
        return;
    }
    // Triggered from the browser/clipboard while another app has focus: Windows won't let us steal focus,
    // so the dialog shows above everything instead of hidden behind the browser.
    let main_focused = app.get_webview_window("main").map(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false)).unwrap_or(false);
    // Windows are built from an async task: building one on the event-loop thread can deadlock WebView2.
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        // A new webview is white until its page paints — start dark when the system is dark.
        let dark = h.get_webview_window("main").and_then(|w| w.theme().ok()) == Some(tauri::Theme::Dark);
        let bg = if dark { tauri::window::Color(0x21, 0x22, 0x27, 255) } else { tauri::window::Color(255, 255, 255, 255) };
        let b = WebviewWindowBuilder::new(&h, &label, WebviewUrl::App("add.html".into()))
            .theme(window_theme(&h.state::<Arc<Manager>>().settings().theme))
            .background_color(bg)
            .title("Add download — FluxDM")
            .inner_size(660.0, 640.0)
            .min_inner_size(520.0, 480.0)
            .center()
            .always_on_top(!main_focused)
            .disable_drag_drop_handler();
        if let Ok(w) = b.build() {
            let _ = w.set_theme(window_theme(&h.state::<Arc<Manager>>().settings().theme));
            let key = label.clone();
            w.on_window_event(move |event| {
                if matches!(event, WindowEvent::Destroyed) { PENDING_ADD.lock().remove(&key); }
            });
            let _ = w.set_focus();
        } else {
            PENDING_ADD.lock().remove(&label);
        }
    });
}

fn pending_add(req: AddRequest) -> (String, bool) {
    let mut pending = PENDING_ADD.lock();
    if req.name.is_none() {
        if let Some((label, existing)) = pending.iter_mut().find(|(_, old)| {
            old.name.is_none() && old.headers == req.headers && old.video == req.video && old.queue == req.queue
        }) {
            if !existing.urls.is_empty() && !req.urls.is_empty() { existing.urls.push('\n'); }
            existing.urls.push_str(&req.urls);
            return (label.clone(), false);
        }
    }
    let label = format!("add-{}", uuid::Uuid::new_v4());
    pending.insert(label.clone(), req);
    (label, true)
}

#[test]
fn incoming_add_forms_keep_each_requests_context() {
    let (first, _) = pending_add(AddRequest { urls: "https://example.invalid/one".into(), headers: "Cookie: first\nReferer: https://example.invalid/page".into(), name: Some("first.zip".into()), ..Default::default() });
    let (second, _) = pending_add(AddRequest { urls: "https://example.invalid/two".into(), headers: "Authorization: second".into(), video: true, queue: Some("video".into()), ..Default::default() });
    assert_ne!(first, second);
    let a = PENDING_ADD.lock().remove(&first).unwrap();
    let b = PENDING_ADD.lock().remove(&second).unwrap();
    assert_eq!(a.name.as_deref(), Some("first.zip"));
    assert!(a.headers.contains("Cookie: first"));
    assert!(a.headers.contains("Referer:"));
    assert!(!a.video);
    assert_eq!(b.urls, "https://example.invalid/two");
    assert_eq!(b.headers, "Authorization: second");
    assert!(b.video);
    assert_eq!(b.queue.as_deref(), Some("video"));
}

#[test]
fn incoming_add_forms_merge_only_matching_contexts_before_and_after_init() {
    let req = AddRequest { urls: "https://example.invalid/one".into(), headers: "Cookie: merge-test".into(), video: true, queue: Some("merge-test".into()), ..Default::default() };
    let (first, created) = pending_add(req.clone());
    assert!(created);
    let mut next = req.clone();
    next.urls = "https://example.invalid/two".into();
    assert_eq!(pending_add(next.clone()), (first.clone(), false));
    {
        let mut pending = PENDING_ADD.lock();
        let form = pending.get_mut(&first).unwrap();
        assert_eq!(form.urls, "https://example.invalid/one\nhttps://example.invalid/two");
        // Loading consumes links but retains the context for later arrivals.
        form.urls.clear();
    }
    assert_eq!(pending_add(next), (first.clone(), false));
    assert_eq!(PENDING_ADD.lock()[&first].urls, "https://example.invalid/two");
    for differing in [
        AddRequest { headers: "Cookie: different".into(), ..req.clone() },
        AddRequest { name: Some("forced.zip".into()), ..req.clone() },
        AddRequest { video: false, ..req.clone() },
        AddRequest { queue: Some("different".into()), ..req.clone() },
    ] {
        let (label, created) = pending_add(differing);
        assert!(created);
        assert_ne!(label, first);
        PENDING_ADD.lock().remove(&label);
    }
    PENDING_ADD.lock().remove(&first);
}

fn toast_window(app: &AppHandle) {
    if app.get_webview_window("toast").is_some() {
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "toast", WebviewUrl::App("toast.html".into()))
        .theme(window_theme(&app.state::<Arc<Manager>>().settings().theme))
        .title("FluxDM")
        .inner_size(360.0, 104.0)
        .decorations(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .focused(false)
        .focusable(false)
        .visible(false)
        .shadow(true)
        .build();
}

fn show_toast(app: &AppHandle, mut payload: Value) {
    payload["gen"] = TOAST_GEN.fetch_add(1, Ordering::SeqCst).wrapping_add(1).into();
    if app.get_webview_window("toast").is_none() {
        TOAST_INBOX.lock().0 = false; // a new page has to say it's listening first
        // Windows are built from an async task: building one on the event-loop thread can deadlock WebView2.
        let h = app.clone();
        tauri::async_runtime::spawn(async move { toast_window(&h) });
    }
    // Emitted in call order, under the lock: a toast can't overtake another or slip past a page that is still loading.
    let mut inbox = TOAST_INBOX.lock();
    if inbox.0 {
        let _ = app.emit_to("toast", "toast", payload);
    } else {
        inbox.1.push(payload);
    }
}

/// The page puts a toast on screen: place and show the window (it never takes focus), and arm the backstop.
#[tauri::command]
fn toast_show(app: AppHandle, gen: u64) {
    TOAST_SHOWN.store(gen, Ordering::SeqCst);
    let Some(w) = app.get_webview_window("toast") else { return };
    if !w.is_visible().unwrap_or(false) {
        if let Ok(Some(mon)) = w.primary_monitor() {
            let wa = mon.work_area();
            let scale = mon.scale_factor();
            let (tw, th) = ((360.0 * scale) as i32, (104.0 * scale) as i32);
            let x = wa.position.x + wa.size.width as i32 - tw - (16.0 * scale) as i32;
            let y = wa.position.y + wa.size.height as i32 - th - (16.0 * scale) as i32;
            let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
        }
        // tao already shows this window without activating it; the webview can still pull focus, so check.
        let prev = win::foreground();
        let _ = w.show();
        let own = w.hwnd().map(|h| h.0 as isize).unwrap_or(0);
        if own != 0 {
            tauri::async_runtime::spawn(async move {
                for ms in [60u64, 250, 700] {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    win::give_back_foreground(own, prev);
                }
            });
        }
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(TOAST_BACKSTOP).await;
        if TOAST_SHOWN.compare_exchange(gen, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            let _ = w.hide();
            let _ = w.emit("toast-reset", ());
        }
    });
}

fn update_chrome(app: &AppHandle, mgr: &Manager) {
    let running = mgr.running_count();
    let (done, total) = {
        let st = mgr.st.lock();
        st.items.iter().filter(|i| i.status == DlStatus::Downloading && i.total_size > 0).fold((0i64, 0i64), |a, i| (a.0 + i.downloaded, a.1 + i.total_size))
    };
    let speed = fmt::speed(*mgr.total_speed.lock());
    let pct = if total > 0 { format!("{:.0}% · ", done as f64 * 100.0 / total as f64) } else { String::new() };
    // Only touch the title, taskbar bar and tray tooltip when their text changed: re-setting an identical
    // tooltip twice a second makes it flicker while the mouse is over the tray icon.
    static LAST: Mutex<(String, i64, String)> = Mutex::new((String::new(), -3, String::new()));
    let title = if running > 0 { format!("{pct}{speed} — FluxDM") } else { "FluxDM".into() };
    let bar = if running == 0 { -1 } else if total > 0 { done * 100 / total } else { -2 }; // -2 = indeterminate
    let tip: String = if running > 0 { format!("FluxDM — {running} running, {pct}{speed}") } else { "FluxDM — idle".into() }.chars().take(120).collect();
    let mut last = LAST.lock();
    if let Some(w) = app.get_webview_window("main") {
        if last.0 != title {
            let _ = w.set_title(&title);
        }
        if last.1 != bar {
            use tauri::window::{ProgressBarState, ProgressBarStatus};
            let state = if running == 0 {
                ProgressBarState { status: Some(ProgressBarStatus::None), progress: None }
            } else if total > 0 {
                ProgressBarState { status: Some(ProgressBarStatus::Normal), progress: Some((done * 100 / total) as u64) }
            } else {
                ProgressBarState { status: Some(ProgressBarStatus::Indeterminate), progress: None }
            };
            let _ = w.set_progress_bar(state);
        }
    }
    if last.2 != tip {
        if let Some(t) = app.tray_by_id("main") {
            let _ = t.set_tooltip(Some(tip.clone()));
        }
    }
    *last = (title, bar, tip);
}

/// Remembers the main window's size and place (same fields the .NET build used, in logical pixels).
fn save_geometry(app: &AppHandle, mgr: &Manager) {
    let Some(w) = app.get_webview_window("main") else { return };
    if !w.is_visible().unwrap_or(false) || w.is_minimized().unwrap_or(false) {
        return;
    }
    let maximized = w.is_maximized().unwrap_or(false);
    let scale = w.scale_factor().unwrap_or(1.0);
    let mut st = mgr.st.lock();
    let extra = &mut st.settings.extra;
    extra.insert("WindowMaximized".into(), json!(maximized));
    if !maximized {
        if let (Ok(p), Ok(s)) = (w.outer_position(), w.inner_size()) {
            extra.insert("WindowLeft".into(), json!((p.x as f64 / scale).round()));
            extra.insert("WindowTop".into(), json!((p.y as f64 / scale).round()));
            extra.insert("WindowWidth".into(), json!((s.width as f64 / scale).round()));
            extra.insert("WindowHeight".into(), json!((s.height as f64 / scale).round()));
        }
    }
    drop(st);
    mgr.mark_dirty();
}

fn restore_geometry(w: &tauri::WebviewWindow, s: &Settings) {
    let num = |k: &str| s.extra.get(k).and_then(|v| v.as_f64());
    if let (Some(width), Some(height)) = (num("WindowWidth"), num("WindowHeight")) {
        if width >= 600.0 && height >= 400.0 {
            let _ = w.set_size(tauri::LogicalSize::new(width, height));
            if let (Some(x), Some(y)) = (num("WindowLeft"), num("WindowTop")) {
                // Only reuse the place if it's still on a connected screen.
                let on_screen = w.available_monitors().unwrap_or_default().iter().any(|m| {
                    let sc = m.scale_factor();
                    let (mx, my) = (m.position().x as f64 / sc, m.position().y as f64 / sc);
                    let (mw, mh) = (m.size().width as f64 / sc, m.size().height as f64 / sc);
                    x + 100.0 > mx && x < mx + mw - 100.0 && y >= my - 10.0 && y < my + mh - 60.0
                });
                if on_screen {
                    let _ = w.set_position(tauri::LogicalPosition::new(x, y));
                }
            }
        }
    }
    if s.extra.get("WindowMaximized").and_then(|v| v.as_bool()).unwrap_or(false) {
        let _ = w.maximize();
    }
}

pub async fn exit_app(app: &AppHandle) {
    let mgr = app.state::<Arc<Manager>>().inner().clone();
    if mgr.exiting.swap(true, Ordering::SeqCst) {
        return;
    }
    save_geometry(app, &mgr);
    let _ = tokio::time::timeout(Duration::from_secs(5), mgr.pause_all()).await;
    let _ = tokio::time::timeout(Duration::from_secs(4), mgr.torrents.shutdown()).await;
    mgr.save();
    api::stop_local();
    api::stop_remote();
    win::keep_awake(false);
    app.exit(0);
}

// ------------------------------------------------------------------ background loops

fn start_clipboard_watch(app: AppHandle, mgr: Arc<Manager>) {
    std::thread::spawn(move || {
        let mut seq = win::clipboard_sequence();
        loop {
            std::thread::sleep(Duration::from_millis(600));
            if mgr.exiting.load(Ordering::Relaxed) {
                return;
            }
            let now = win::clipboard_sequence();
            if now == seq {
                continue;
            }
            seq = now;
            let s = mgr.settings();
            if !s.clipboard_monitor {
                continue;
            }
            let Some(text) = win::clipboard_text() else { continue };
            let text = text.trim().to_string();
            {
                let mut last = LAST_CLIP.lock();
                if *last == text {
                    continue;
                }
                *last = text.clone();
            }
            if text.contains('\n') {
                continue;
            }
            let magnet = net::is_magnet(&text);
            if !magnet && (text.len() > 4000 || !net::is_http_url(&text) || !net::looks_downloadable(&text, &s.clipboard_extensions)) {
                continue;
            }
            if mgr.st.lock().items.iter().any(|i| i.url == text) {
                continue;
            }
            show_add(&app, AddRequest { urls: text, ..Default::default() });
        }
    });
}

/// (Re)starts the browser bridge and/or the phone remote to match the settings. `quiet` = at startup:
/// a blocked remote only gets a toast; when it's just been switched on, the window offers the firewall fix.
async fn apply_servers(app: &AppHandle, mgr: &Arc<Manager>, quiet: bool, local: bool, remote: bool) {
    let s = mgr.settings();
    if local {
        api::stop_local();
        if s.local_api_enabled && !api::start_local(mgr.clone(), app.clone(), s.local_api_port as u16).await {
            mgr.toast("Browser bridge is off", &format!("Port {} is in use by another program.", s.local_api_port), None);
        }
    }
    if !remote {
        return;
    }
    api::stop_remote();
    if s.remote_enabled {
        if !api::start_remote(mgr.clone(), app.clone(), s.remote_port as u16).await {
            mgr.toast("Phone remote is off", &format!("Port {} is in use.", s.remote_port), None);
            return;
        }
        let port = s.remote_port;
        let (m, h) = (mgr.clone(), app.clone());
        tauri::async_runtime::spawn_blocking(move || {
            if win::firewall_rule_exists(REMOTE_RULE, port) {
                return;
            }
            if quiet {
                m.toast("Phone remote is blocked", "Windows Firewall blocks your phone. Settings → Phone remote → Allow through firewall.", None);
            } else {
                let _ = h.emit_to("main", "offer-firewall", json!({ "kind": "remote" }));
            }
        });
    }
}

// ------------------------------------------------------------------ commands: main window

#[tauri::command]
fn init(mgr: M) -> Value {
    let s = mgr.settings();
    json!({
        "settings": s,
        "categories": CATEGORIES,
        "formats": video::FORMATS.iter().map(|(k, l)| json!({ "key": k, "label": l })).collect::<Vec<_>>(),
        "version": update::current_version(),
        "dataDir": store::data_dir(),
    })
}

#[tauri::command]
fn ready(mgr: M) {
    mgr.reset_sent();
    mgr.push_list();
}

#[tauri::command]
fn start(mgr: M, ids: Vec<String>) {
    for id in ids {
        mgr.start(&id);
    }
    mgr.push_list();
}

#[tauri::command]
async fn pause(mgr: M<'_>, ids: Vec<String>) -> R<()> {
    let m = mgr.inner().clone();
    futures_util::future::join_all(ids.iter().map(|id| m.pause(id))).await;
    m.push_list();
    Ok(())
}

#[tauri::command]
async fn toggle(mgr: M<'_>, ids: Vec<String>) -> R<()> {
    let m = mgr.inner().clone();
    for id in ids {
        if m.is_running(&id) {
            let m2 = m.clone();
            tauri::async_runtime::spawn(async move { m2.pause(&id).await });
        } else if m.with_item(&id, |i| i.status != DlStatus::Completed).unwrap_or(false) {
            m.start(&id);
        }
    }
    m.push_list();
    Ok(())
}

#[tauri::command]
async fn pause_all(mgr: M<'_>) -> R<()> {
    mgr.inner().pause_all().await;
    Ok(())
}

#[tauri::command]
async fn remove(mgr: M<'_>, ids: Vec<String>, delete_files: bool) -> R<()> {
    let m = mgr.inner().clone();
    for id in ids {
        m.remove(&id, delete_files).await;
    }
    Ok(())
}

#[tauri::command]
async fn restart(mgr: M<'_>, ids: Vec<String>) -> R<()> {
    let m = mgr.inner().clone();
    for id in ids {
        m.restart(&id).await;
    }
    Ok(())
}

#[tauri::command]
async fn clear_finished(mgr: M<'_>) -> R<usize> {
    let m = mgr.inner().clone();
    let cleared: Vec<Item> = {
        let mut st = m.st.lock();
        let (done, keep): (Vec<Item>, Vec<Item>) = std::mem::take(&mut st.items).into_iter().partition(|i| i.status == DlStatus::Completed);
        st.items = keep;
        done
    };
    // A finished torrent may still be seeding: take it out of the engine too (its files stay), or it would keep
    // uploading with no row left to see or stop it.
    for it in cleared.iter().filter(|i| i.is_torrent) {
        m.torrents.remove(&m, it, false).await;
        let still_used = m.st.lock().items.iter().any(|o| o.torrent_file == it.torrent_file);
        if !still_used && !it.torrent_file.is_empty() {
            let _ = std::fs::remove_file(&it.torrent_file);
        }
    }
    m.save();
    m.push_list();
    Ok(cleared.len())
}

/// Changes simple fields of one or more downloads.
#[tauri::command]
fn set_items(mgr: M, ids: Vec<String>, patch: Value) {
    let running_q: Vec<String> = mgr.st.lock().queues.iter().filter(|q| q.running).map(|q| q.name.clone()).collect();
    for id in &ids {
        let running = mgr.is_running(id);
        mgr.with_item(id, |it| {
            if let Some(v) = patch["connections"].as_i64() {
                it.set_connections(v as i32);
            }
            if let Some(v) = patch["limitKBps"].as_i64() {
                it.speed_limit_kbps = v.max(0) as i32;
            }
            if let Some(v) = patch["sequential"].as_bool() {
                it.sequential = v;
            }
            if let Some(v) = patch["openWhenDone"].as_bool() {
                it.open_when_done = v;
            }
            if let Some(v) = patch["uploadLimitKBps"].as_i64() {
                it.upload_limit_kbps = v.max(-1) as i32;
            }
            if let Some(v) = patch["seedingLimitKBps"].as_i64() {
                it.seeding_limit_kbps = v.max(-1) as i32;
            }
            if let Some(v) = patch["hash"].as_str() {
                it.expected_hash = v.trim().to_string();
            }
            if let Some(v) = patch["tags"].as_array() {
                it.tags = v.iter().filter_map(|t| t.as_str().map(|s| s.trim().to_string())).filter(|t| !t.is_empty()).collect();
            }
            if !patch["queue"].is_null() {
                let q = patch["queue"].as_str().unwrap_or("").to_string();
                it.queue_name = q.clone();
                it.queue_order = chrono::Local::now().timestamp_micros();
                if !q.is_empty() && running_q.contains(&q) && !running && matches!(it.status, DlStatus::Paused | DlStatus::Error) {
                    it.status = DlStatus::Queued;
                } else if q.is_empty() && it.status == DlStatus::Queued {
                    it.status = DlStatus::Paused;
                }
            }
        });
    }
    mgr.save();
    mgr.push_list();
}

#[tauri::command]
fn add_mirrors(mgr: M, id: String, text: String) -> R<usize> {
    let it = mgr.item(&id).ok_or("gone")?;
    if it.is_video || it.is_torrent {
        return Err("Backup links are for normal file downloads, not video pages or torrents.".into());
    }
    let mut added: Vec<String> = vec![];
    for u in text.split(['\n', ' ']).map(str::trim).filter(|u| net::is_http_url(u)) {
        if u != it.url && !it.mirrors.iter().any(|m| m == u) && !added.iter().any(|a| a == u) {
            added.push(u.to_string());
        }
    }
    if added.is_empty() {
        return Err("No new http(s) links found.".into());
    }
    let n = added.len();
    mgr.with_item(&id, |it| it.mirrors.extend(added));
    mgr.save();
    mgr.push_list();
    Ok(n)
}

#[tauri::command]
async fn probe(mgr: M<'_>, url: String, headers: String) -> R<Value> {
    let s = mgr.settings();
    let p = net::probe(&url, &headers, &s.user_agent, 15).await.map_err(|e| e.msg)?;
    Ok(json!({ "total": p.total, "resumable": p.resumable, "fileName": p.file_name, "sha256": p.sha256 }))
}

#[tauri::command]
async fn refresh_link(mgr: M<'_>, id: String, url: String, restart: bool) -> R<()> {
    let m = mgr.inner().clone();
    m.pause(&id).await;
    m.with_item(&id, |it| it.url = url);
    if m.with_item(&id, |it| it.status == DlStatus::Completed).unwrap_or(true) {
        m.save();
        return Ok(());
    }
    if restart {
        m.restart(&id).await;
    } else {
        m.start(&id);
    }
    Ok(())
}

#[tauri::command]
fn details(mgr: M, id: String) -> Value {
    let Some(it) = mgr.item(&id) else { return Value::Null };
    let total = it.total_size.max(1) as f64;
    let segs: Vec<Value> = it
        .segments
        .lock()
        .iter()
        .map(|s| {
            let s = s.lock();
            json!([s.start as f64 / total, (s.end + 1) as f64 / total, s.position() as f64 / total, s.active])
        })
        .collect();
    let s = mgr.settings();
    let run = mgr.run_of(&id);
    let seg_info = if it.status == DlStatus::Downloading {
        let throttled = run.map(|r| r.throttled.load(Ordering::Relaxed)).unwrap_or(0);
        let extra = if throttled > 0 { format!(" · server allows ~{}", (it.connections - throttled).max(1)) } else { String::new() };
        format!("{} live · {} segments{extra}", it.active_connections, segs.len())
    } else if !segs.is_empty() {
        format!("{} segments", segs.len())
    } else {
        String::new()
    };
    json!({
        "segments": segs,
        "history": it.speed_history,
        "segInfo": seg_info,
        "torrent": if it.is_torrent { mgr.torrents.details(&it) } else { Value::Null },
        "cover": covers::cover_url(&it, &s),
    })
}

#[tauri::command]
fn open_item(mgr: M, id: String) {
    if let Some(it) = mgr.item(&id) {
        if it.status == DlStatus::Completed {
            win::open(&it.full_path());
        }
    }
}

#[tauri::command]
fn show_item(mgr: M, id: String) {
    if let Some(it) = mgr.item(&id) {
        let p = if it.status == DlStatus::Completed || it.is_torrent || it.is_video { it.full_path() } else { it.part_path() };
        win::show_in_folder(&p);
    }
}

#[tauri::command]
fn copy_text(text: String) -> bool {
    *LAST_CLIP.lock() = text.trim().to_string();
    win::set_clipboard_text(&text)
}

#[tauri::command]
fn clipboard_url() -> Option<String> {
    let t = win::clipboard_text()?.trim().to_string();
    (net::is_http_url(&t) || net::is_magnet(&t)).then_some(t)
}

#[tauri::command]
async fn sha256(mgr: M<'_>, id: String) -> R<String> {
    let it = mgr.item(&id).ok_or("gone")?;
    let path = it.full_path();
    tauri::async_runtime::spawn_blocking(move || util::hash_file(&path, util::Algo::Sha256, |_| {}, &AtomicBool::new(false)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_global_limit(mgr: M, kbps: i32) {
    mgr.st.lock().settings.global_speed_limit_kbps = kbps.max(0);
    mgr.mark_dirty();
    mgr.push_list();
}

#[tauri::command]
fn parse_speed(text: String) -> Option<i32> {
    fmt::parse_speed(&text)
}

#[tauri::command]
fn free_space(folder: String) -> Option<i64> {
    win::free_space(&folder)
}

#[tauri::command]
fn run_post_action(app: AppHandle, action: String, force: bool) {
    let a = match action.as_str() {
        "Exit" => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move { exit_app(&h).await });
            return;
        }
        "Sleep" => PostAction::Sleep,
        "Hibernate" => PostAction::Hibernate,
        "Shutdown" => PostAction::Shutdown,
        "Restart" => PostAction::Restart,
        _ => return,
    };
    let mgr = app.state::<Arc<Manager>>();
    mgr.save();
    win::run_post_action(a, force);
}

#[tauri::command]
async fn exit(app: AppHandle) {
    exit_app(&app).await;
}

#[tauri::command]
fn toast_click(app: AppHandle, reveal: Option<String>) {
    TOAST_SHOWN.store(0, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window("toast") {
        let _ = w.hide();
    }
    match reveal {
        Some(p) if !p.is_empty() => win::show_in_folder(&p),
        _ => show_main(&app),
    }
}

/// The notification page is listening: hand over the toasts that came while it loaded.
#[tauri::command]
fn toast_ready() -> Vec<Value> {
    let mut inbox = TOAST_INBOX.lock();
    inbox.0 = true;
    std::mem::take(&mut inbox.1)
}

/// The page is done with toast `gen` and has nothing else to show.
#[tauri::command]
fn toast_hide(app: AppHandle, gen: u64) {
    if TOAST_SHOWN.compare_exchange(gen, 0, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }
    if let Some(w) = app.get_webview_window("toast") {
        let _ = w.hide();
    }
}

#[tauri::command]
fn open_add(app: AppHandle, urls: Option<String>, video: Option<bool>, queue: Option<String>) {
    show_add(&app, AddRequest { urls: urls.unwrap_or_default(), video: video.unwrap_or(false), queue, ..Default::default() });
}

/// A .torrent file dropped onto a window: saved to the data folder, then opened in the Add window.
#[tauri::command]
fn dropped_torrent(app: AppHandle, bytes: Vec<u8>) -> R<()> {
    let meta = torrent::parse(&bytes).map_err(|e| e.msg)?;
    let dir = torrent::dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join(format!("{}.torrent", meta.hash.to_lowercase()));
    std::fs::write(&p, &bytes).map_err(|e| e.to_string())?;
    show_add(&app, AddRequest { urls: p.to_string_lossy().into_owned(), ..Default::default() });
    Ok(())
}

// ------------------------------------------------------------------ commands: add window

#[tauri::command]
fn add_init(window: tauri::WebviewWindow, mgr: M) -> Value {
    let req = PENDING_ADD.lock().get_mut(window.label()).map(|r| {
        let req = r.clone();
        r.urls.clear();
        req
    }).unwrap_or_default();
    let s = mgr.settings();
    let queues: Vec<String> = mgr.st.lock().queues.iter().map(|q| q.name.clone()).collect();
    json!({
        "request": req,
        "defaultFolder": s.default_folder,
        "categorySubfolders": s.category_subfolders,
        "connections": s.default_connections,
        "videoFormat": s.default_video_format,
        "seedRatio": s.torrent_seed_ratio,
        "queues": queues,
        "formats": video::FORMATS.iter().map(|(k, l)| json!({ "key": k, "label": l })).collect::<Vec<_>>(),
        "ytInstalled": video::is_installed(),
    })
}

#[tauri::command]
fn add_take_links(window: tauri::WebviewWindow) -> String {
    PENDING_ADD.lock().get_mut(window.label()).map(|r| std::mem::take(&mut r.urls)).unwrap_or_default()
}

#[tauri::command]
fn classify(mgr: M, text: String) -> Value {
    // Every line → links (batch ranges expanded), and what kind they are.
    let mut urls: Vec<String> = vec![];
    for l in text.split('\n').map(|l| l.trim().trim_matches('"')).filter(|l| !l.is_empty()) {
        let expanded = if net::is_magnet(l) { vec![l.to_string()] } else { net::expand_pattern(l) };
        for u in expanded {
            if net::is_download_source(&u) && !urls.contains(&u) {
                urls.push(u);
            }
        }
    }
    let torrent = !urls.is_empty() && urls.iter().all(|u| net::is_torrent_source(u));
    let video = !urls.is_empty() && urls.iter().all(|u| video::is_video_site(u));
    let st = mgr.st.lock();
    let dups: Vec<String> = urls.iter().filter_map(|u| st.items.iter().find(|i| &i.url == u).map(|i| i.status_text())).collect();
    json!({
        "urls": urls,
        "torrent": torrent,
        "video": video,
        "dups": dups,
        "names": urls.iter().map(|u| net::name_from_url(u, true)).collect::<Vec<_>>(),
        "magnetName": urls.first().filter(|u| net::is_magnet(u)).and_then(|u| net::magnet_name(u)),
    })
}

#[tauri::command]
fn folder_for(mgr: M, name: String) -> String {
    Manager::folder_for(&mgr.settings(), &name)
}

#[tauri::command]
async fn video_probe(mgr: M<'_>, url: String) -> R<Value> {
    let s = mgr.settings();
    video::probe(&url, &s).await.map_err(|e| e.msg)
}

#[tauri::command]
async fn torrent_info(mgr: M<'_>, source: String, headers: Option<String>) -> R<Value> {
    let m = mgr.inner().clone();
    let r = tokio::time::timeout(Duration::from_secs(90), async { m.torrents.info(&m, &source, headers.as_deref().unwrap_or("")).await }).await;
    match r {
        Ok(v) => {
            let mut v = v.map_err(|e| e.msg)?;
            let hash = v["infoHash"].as_str().unwrap_or("").to_lowercase();
            if let Some(dup) = m.st.lock().items.iter().find(|i| i.is_torrent && i.info_hash.to_lowercase() == hash) {
                v["dup"] = json!(dup.status_text());
            }
            Ok(v)
        }
        Err(_) => Err("no peers answered in 90 s".into()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddSubmit {
    urls: Vec<String>,
    mode: String, // "file" | "video" | "torrent"
    name: String,
    name_edited: bool,
    folder: String,
    folder_edited: bool,
    connections: i32,
    limit_kbps: i32,
    /// Torrents: their own upload limits while downloading / once finished (KB/s, 0 = none of its own, -1 = no upload).
    #[serde(default)]
    upload_limit_kbps: i32,
    #[serde(default)]
    seeding_limit_kbps: i32,
    queue: String,
    headers: String,
    hash: String,
    mirrors: Vec<String>,
    video_format: String,
    playlist: bool,
    video_title: Option<String>,
    probe_total: Option<i64>,
    probe_resumable: Option<bool>,
    torrent_name: Option<String>,
    torrent_hash: Option<String>,
    torrent_file: Option<String>,
    skipped: Option<Vec<i32>>,
    wanted_size: Option<i64>,
    #[serde(default)]
    open_when_done: bool,
    start: bool,
}

#[tauri::command]
fn add_submit(window: tauri::WebviewWindow, mgr: M, req: AddSubmit) -> R<()> {
    if req.urls.is_empty() {
        return Err("Paste at least one link.".into());
    }
    let s = mgr.settings();
    let folder = if req.folder.trim().is_empty() { s.default_folder.clone() } else { req.folder.trim().to_string() };
    if !std::path::Path::new(&folder).is_absolute() {
        return Err("The save folder path isn't valid.".into());
    }
    let single = req.urls.len() == 1;
    let mut items: Vec<Item> = vec![];
    match req.mode.as_str() {
        "torrent" => {
            for u in &req.urls {
                let info = single && req.torrent_hash.is_some();
                let mut it = mgr.create_torrent_item(
                    u,
                    req.torrent_name.as_deref().filter(|_| info),
                    req.folder_edited.then_some(folder.as_str()),
                    if info { req.skipped.clone() } else { None },
                );
                if info {
                    it.torrent_file = req.torrent_file.clone().unwrap_or_default();
                    it.info_hash = req.torrent_hash.clone().unwrap_or_default();
                    it.total_size = req.wanted_size.unwrap_or(-1);
                }
                it.speed_limit_kbps = req.limit_kbps;
                it.upload_limit_kbps = req.upload_limit_kbps.max(-1);
                it.seeding_limit_kbps = req.seeding_limit_kbps.max(-1);
                it.queue_name = req.queue.clone();
                items.push(it);
            }
        }
        "video" => {
            for u in &req.urls {
                let title = if single { req.video_title.as_deref() } else { None };
                let mut it = mgr.create_video_item(u, title, &req.video_format, req.playlist, req.folder_edited.then_some(folder.as_str()));
                it.set_connections(req.connections);
                it.speed_limit_kbps = req.limit_kbps;
                it.queue_name = req.queue.clone();
                items.push(it);
            }
        }
        _ => {
            let hash = req.hash.trim().to_string();
            if !hash.is_empty() && util::algo_for(&hash).is_none() {
                return Err("The checksum must be an MD5, SHA-1, SHA-256 or SHA-512 hex string (or left empty).".into());
            }
            let probed = single && req.probe_total.is_some();
            for u in &req.urls {
                let name = if single && !req.name.trim().is_empty() { req.name.trim().to_string() } else { net::name_from_url(u, true).unwrap_or_else(|| "download".into()) };
                let dir = if single || req.folder_edited || !s.category_subfolders { folder.clone() } else { Manager::folder_for(&s, &name) };
                let mut it = Item {
                    url: u.clone(),
                    save_folder: dir.clone(),
                    speed_limit_kbps: req.limit_kbps,
                    queue_name: req.queue.clone(),
                    headers: req.headers.trim().to_string(),
                    expected_hash: if single { hash.clone() } else { String::new() },
                    auto_name: !(single && (req.name_edited || probed)),
                    mirrors: if single { req.mirrors.iter().filter(|m| *m != u).cloned().collect() } else { vec![] },
                    ..Default::default()
                };
                it.set_connections(req.connections);
                it.file_name = mgr.unique_name(&dir, &name, None, &items);
                if probed {
                    it.total_size = req.probe_total.unwrap_or(-1);
                    it.resumable = req.probe_resumable.unwrap_or(false);
                }
                items.push(it);
            }
        }
    }
    for it in &mut items {
        it.open_when_done = req.open_when_done;
        it.headers = req.headers.trim().to_string();
    }
    mgr.inner().add(items, req.start);
    let _ = window.close();
    Ok(())
}

// ------------------------------------------------------------------ commands: torrents

#[tauri::command]
async fn torrent_recheck(mgr: M<'_>, ids: Vec<String>) -> R<()> {
    let m = mgr.inner().clone();
    // One torrent that can't be checked must not leave the others in the selection unchecked.
    let mut failed: Vec<String> = vec![];
    for id in ids {
        let Some(it) = m.item(&id) else { continue };
        if it.status == DlStatus::Verifying {
            continue;
        }
        let resume = m.is_running(&id) || matches!(it.status, DlStatus::Queued | DlStatus::Completed);
        m.pause(&id).await;
        m.with_item(&id, |it| {
            it.set_status(DlStatus::Verifying);
            it.verify_progress = 0.0;
            it.error_message.clear();
            it.note.clear();
        });
        m.push_list();
        match m.torrents.recheck(&m, &id).await {
            Ok(pct) => {
                let done = pct >= 99.999;
                m.with_item(&id, |it| {
                    it.downloaded = (pct.clamp(0.0, 100.0) / 100.0 * it.total_size as f64) as i64;
                    it.status = if done { DlStatus::Completed } else { DlStatus::Paused };
                    if done && it.completed_at.is_none() {
                        it.completed_at = Some(chrono::Local::now());
                    }
                });
                if !done && resume {
                    m.start(&id);
                }
            }
            Err(e) => {
                // Nothing was checked: a finished torrent stays finished (Paused would make Resume download it again).
                let before = if it.status == DlStatus::Completed { DlStatus::Completed } else { DlStatus::Paused };
                m.with_item(&id, |x| x.status = before);
                failed.push(format!("Couldn't recheck “{}”: {}", it.file_name, e.msg));
            }
        }
        m.save();
    }
    m.push_list();
    if failed.is_empty() { Ok(()) } else { Err(failed.join("\n")) }
}

/// Asks trackers and DHT for peers again: re-adding a running torrent makes librqbit announce afresh.
#[tauri::command]
async fn torrent_reannounce(mgr: M<'_>, ids: Vec<String>) -> R<usize> {
    let m = mgr.inner().clone();
    let mut n = 0;
    for id in ids {
        if !m.is_running(&id) || !m.with_item(&id, |i| i.is_torrent).unwrap_or(false) {
            continue;
        }
        m.pause(&id).await;
        if let Some(it) = m.item(&id) {
            m.torrents.remove(&m, &it, false).await;
        }
        m.start(&id);
        n += 1;
    }
    Ok(n)
}

#[tauri::command]
async fn torrent_move(mgr: M<'_>, ids: Vec<String>, folder: String, restart_if_unfinished: Option<bool>) -> R<()> {
    let m = mgr.inner().clone();
    for id in ids {
        let Some(it) = m.item(&id) else { continue };
        if it.save_folder.trim_end_matches('\\').eq_ignore_ascii_case(folder.trim_end_matches('\\')) {
            continue;
        }
        // An unfinished torrent's files are pre-sized placeholders (the full torrent size, mostly empty), so copying
        // them to another drive writes and re-verifies every byte. Starting over there is far cheaper.
        let fresh = it.status != DlStatus::Completed && !torrent::same_drive(&it.save_folder, &folder);
        if fresh && restart_if_unfinished != Some(true) {
            return Err(format!("“{}” isn't finished, and moving it to another drive would copy its whole placeholder files. Choose “start over there” instead.", it.file_name));
        }
        let resume = m.is_running(&id);
        torrent::check_move_destination(&it, &folder).map_err(|e| e.msg)?;
        m.pause(&id).await;
        m.with_item(&id, |it| it.note = if fresh { "clearing old partial files…".into() } else { "moving files…".into() });
        let r = if fresh {
            m.torrents.remove(&m, &it, true).await;
            Ok(())
        } else {
            m.torrents.move_data(&m, &id, &folder).await
        };
        m.with_item(&id, |it| {
            it.note.clear();
            if r.is_ok() {
                it.save_folder = folder.clone();
                if fresh {
                    it.downloaded = 0;
                    it.verify_progress = 0.0;
                }
            }
        });
        if resume {
            m.start(&id);
        }
        m.save();
        r.map_err(|e| format!("Couldn't move “{}”: {}", it.file_name, e.msg))?;
    }
    Ok(())
}

#[tauri::command]
async fn torrent_add_trackers(mgr: M<'_>, ids: Vec<String>, text: String) -> R<usize> {
    let urls: Vec<String> = text
        .split([' ', '\n', '\r', '\t', ','])
        .filter(|u| url::Url::parse(u).map(|x| matches!(x.scheme(), "udp" | "http" | "https")).unwrap_or(false))
        .map(str::to_string)
        .collect();
    if urls.is_empty() {
        return Err("No udp:// or http(s):// tracker URLs found.".into());
    }
    let m = mgr.inner().clone();
    for id in &ids {
        m.with_item(id, |it| {
            for u in &urls {
                if !it.extra_trackers.iter().any(|t| t.eq_ignore_ascii_case(u)) {
                    it.extra_trackers.push(u.clone());
                }
            }
        });
        // Trackers are handed to the engine when a torrent is added: re-add running ones.
        if m.is_running(id) {
            m.pause(id).await;
            if let Some(it) = m.item(id) {
                m.torrents.remove(&m, &it, false).await;
            }
            m.start(id);
        }
    }
    m.save();
    Ok(urls.len())
}

#[tauri::command]
fn torrent_copy(mgr: M, ids: Vec<String>, what: String) -> bool {
    let lines: Vec<String> = ids
        .iter()
        .filter_map(|id| mgr.item(id))
        .filter(|i| i.is_torrent)
        .map(|i| match what.as_str() {
            "hash" => i.info_hash.to_lowercase(),
            "magnet" => torrent::Service::magnet_for(&i),
            _ => i.file_name.clone(),
        })
        .filter(|l| !l.is_empty())
        .collect();
    !lines.is_empty() && copy_text(lines.join("\r\n"))
}

#[tauri::command]
fn torrent_export(mgr: M, ids: Vec<String>, dest: String, is_folder: bool) -> R<usize> {
    let items: Vec<Item> = ids.iter().filter_map(|id| mgr.item(id)).filter(|i| std::path::Path::new(&i.torrent_file).is_file()).collect();
    if items.is_empty() {
        return Err("The torrent's info isn't known yet — start it once first.".into());
    }
    for it in &items {
        let target: std::path::PathBuf = if is_folder { std::path::Path::new(&dest).join(format!("{}.torrent", net::sanitize(&it.file_name))) } else { dest.clone().into() };
        std::fs::copy(&it.torrent_file, target).map_err(|e| e.to_string())?;
    }
    Ok(items.len())
}

/// 0 = don't download, 1 Low, 2 Normal, 3 High, 4 Maximum.
#[tauri::command]
async fn torrent_file_priority(mgr: M<'_>, id: String, index: i32, level: u8) -> R<()> {
    let m = mgr.inner().clone();
    m.torrents.set_file_priority(&m, &id, index, level).await;
    m.push_list();
    Ok(())
}

#[tauri::command]
async fn torrent_file(mgr: M<'_>, id: String, index: i32, wanted: bool) -> R<()> {
    let m = mgr.inner().clone();
    m.torrents.set_file_wanted(&m, &id, index, wanted).await;
    m.push_list();
    Ok(())
}

// ------------------------------------------------------------------ commands: icons

#[tauri::command]
async fn icon_find(mgr: M<'_>, id: String, name: String) -> R<Option<String>> {
    let m = mgr.inner().clone();
    Ok(covers::lookup(&m, &id, Some(name)).await)
}

#[tauri::command]
fn icon_choose(mgr: M, ids: Vec<String>, path: String) -> R<()> {
    for id in ids {
        covers::use_image(mgr.inner(), &id, &path).map_err(|e| format!("Couldn't use that picture: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
fn icon_mode(mgr: M, ids: Vec<String>, mode: String) {
    for id in ids {
        covers::set_mode(mgr.inner(), &id, &mode);
    }
}

#[tauri::command]
fn clean_title(name: String) -> String {
    covers::clean_title(&name)
}

// ------------------------------------------------------------------ commands: queues

#[tauri::command]
fn queues(mgr: M) -> Value {
    let st = mgr.st.lock();
    let list: Vec<Value> = st
        .queues
        .iter()
        .map(|q| {
            let mut v = serde_json::to_value(q).unwrap_or(Value::Null);
            v["Running"] = json!(q.running);
            v["Members"] = json!(Manager::queue_members(&st, &q.name)
                .iter()
                .filter_map(|id| st.items.iter().find(|i| &i.id == id))
                .map(|i| json!({ "id": i.id, "name": i.file_name, "status": i.status_text(), "size": if i.total_size > 0 { fmt::size(i.total_size) } else { "?".into() } }))
                .collect::<Vec<_>>());
            v
        })
        .collect();
    json!(list)
}

#[tauri::command]
fn queue_save(mgr: M, old_name: String, def: Value) -> R<()> {
    let name = def["Name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err("A queue needs a name.".into());
    }
    for key in ["StartAt", "StopAt"] {
        let enabled = def[format!("{key}Enabled")].as_bool().unwrap_or(false);
        if enabled && parse_hhmm(def[key].as_str().unwrap_or("")).is_none() {
            return Err("Times must look like 02:00 or 23:30.".into());
        }
    }
    if name != old_name {
        if mgr.st.lock().queues.iter().any(|q| q.name.eq_ignore_ascii_case(&name) && q.name != old_name) {
            return Err("Another queue already has that name.".into());
        }
        mgr.rename_queue(&old_name, &name);
    }
    let mut parsed: QueueDef = serde_json::from_value(def).map_err(|e| e.to_string())?;
    {
        let mut st = mgr.st.lock();
        let Some(q) = st.queues.iter_mut().find(|q| q.name == name) else { return Err("That queue no longer exists.".into()) };
        parsed.name = name.clone();
        parsed.running = q.running;
        parsed.started_ids = q.started_ids.clone();
        parsed.last_start_fire = q.last_start_fire;
        parsed.last_stop_fire = q.last_stop_fire;
        parsed.max_concurrent = parsed.max_concurrent.clamp(1, 20);
        parsed.speed_limit_kbps = parsed.speed_limit_kbps.max(0);
        *q = parsed;
    }
    mgr.save();
    mgr.push_list();
    Ok(())
}

#[tauri::command]
fn queue_new(mgr: M) -> String {
    let name = {
        let mut st = mgr.st.lock();
        let mut n = 2;
        while st.queues.iter().any(|q| q.name == format!("Queue {n}")) {
            n += 1;
        }
        let name = format!("Queue {n}");
        st.queues.push(QueueDef { name: name.clone(), ..Default::default() });
        name
    };
    mgr.save();
    mgr.push_list();
    name
}

#[tauri::command]
fn queue_delete(mgr: M, name: String) -> R<()> {
    if mgr.st.lock().queues.len() <= 1 {
        return Err("Keep at least one queue.".into());
    }
    mgr.inner().delete_queue(&name);
    mgr.save();
    mgr.push_list();
    Ok(())
}

#[tauri::command]
fn queue_run(mgr: M, name: String, on: bool) {
    if on {
        mgr.inner().start_queue(&name);
    } else {
        mgr.inner().stop_queue(&name);
    }
    mgr.push_list();
}

#[tauri::command]
fn queue_move(mgr: M, name: String, id: String, dir: i32) {
    {
        let mut st = mgr.st.lock();
        let list = Manager::queue_members(&st, &name);
        let Some(i) = list.iter().position(|x| *x == id) else { return };
        let j = i as i32 + dir;
        if j < 0 || j >= list.len() as i32 {
            return;
        }
        let (a, b) = (list[i].clone(), list[j as usize].clone());
        let oa = st.items.iter().find(|x| x.id == a).map(|x| x.queue_order).unwrap_or(0);
        let ob = st.items.iter().find(|x| x.id == b).map(|x| x.queue_order).unwrap_or(0);
        let (na, nb) = if oa == ob { if dir > 0 { (ob, oa - 1) } else { (ob - 1, oa) } } else { (ob, oa) };
        for it in st.items.iter_mut() {
            if it.id == a {
                it.queue_order = na;
            } else if it.id == b {
                it.queue_order = nb;
            }
        }
    }
    mgr.mark_dirty();
}

// ------------------------------------------------------------------ commands: settings & stats

#[tauri::command]
fn get_settings(mgr: M) -> Value {
    let s = mgr.settings();
    json!({
        "settings": s,
        "magnet": { "ours": win::is_magnet_handler(), "name": win::magnet_handler_name() },
        "remoteRunning": api::remote_running(),
        "dataDir": store::data_dir(),
    })
}

#[tauri::command]
async fn save_settings(app: AppHandle, mgr: M<'_>, settings: Settings) -> R<()> {
    let mut new = settings;
    // Unknown fields are kept for the .NET version (all PascalCase); anything else is a stray UI key.
    new.extra.retain(|k, _| k.starts_with(|c: char| c.is_ascii_uppercase()));
    std::fs::create_dir_all(&new.default_folder).map_err(|_| "The default save folder can't be created.".to_string())?;
    if new.speed_schedule_enabled && (parse_hhmm(&new.schedule_from).is_none() || parse_hhmm(&new.schedule_to).is_none()) {
        return Err("Speed schedule times must look like 09:00 or 23:30.".into());
    }
    if new.remote_enabled && (new.remote_pin.len() < 4 || !new.remote_pin.chars().all(|c| c.is_ascii_digit())) {
        return Err("The phone remote PIN must be at least 4 digits.".into());
    }
    new.torrent_seed_ratio = new.torrent_seed_ratio.clamp(0.0, 100.0);
    new.torrent_port = if new.torrent_port == 0 { 0 } else { new.torrent_port.clamp(1024, 65535) };
    new.remote_port = new.remote_port.clamp(1024, 65535);
    new.local_api_port = new.local_api_port.clamp(1024, 65535);
    new.default_connections = new.default_connections.clamp(1, MAX_CONNECTIONS);
    new.max_retries = new.max_retries.clamp(0, 1000);
    new.read_timeout_sec = new.read_timeout_sec.clamp(5, 600);
    new.queue_retries = new.queue_retries.clamp(0, 100);
    new.global_speed_limit_kbps = new.global_speed_limit_kbps.max(0);

    let m = mgr.inner().clone();
    let old = m.settings();
    // Things the settings window doesn't edit stay as they are.
    new.magnet_handler_backup = old.magnet_handler_backup.clone();
    new.columns = old.columns.clone();
    new.columns_layout_version = old.columns_layout_version;
    new.sidebar_width = old.sidebar_width;
    new.details_height = old.details_height;
    m.st.lock().settings = new.clone();
    m.save();

    if old.start_with_windows != new.start_with_windows {
        win::set_start_with_windows(new.start_with_windows);
    }
    // Only restart the server whose own settings changed (the PIN is read per request, no restart needed).
    let local = old.local_api_enabled != new.local_api_enabled || old.local_api_port != new.local_api_port;
    let remote = old.remote_enabled != new.remote_enabled || old.remote_port != new.remote_port;
    if local || remote {
        apply_servers(&app, &m, false, local, remote).await;
    }
    if old.show_icons != new.show_icons || old.lookup_covers_online != new.lookup_covers_online {
        covers::refresh_all(&m);
    }
    apply_window_theme(&app, &new.theme);
    let _ = app.emit("theme-changed", &new.theme);
    let _ = app.emit("settings-changed", json!({}));
    m.push_list();
    Ok(())
}

/// The window's column layout / panel sizes (saved with the settings).
#[tauri::command]
fn save_layout(mgr: M, columns: std::collections::HashMap<String, ColumnState>, sidebar: f64, details: f64, split: Option<f64>) {
    {
        let mut st = mgr.st.lock();
        st.settings.columns = columns;
        st.settings.columns_layout_version = 3;
        st.settings.sidebar_width = sidebar;
        st.settings.details_height = details;
        if let Some(f) = split.filter(|f| *f > 0.0 && *f < 1.0) {
            st.settings.extra.insert("DetailsSplit".into(), json!(f));
        }
    }
    mgr.mark_dirty();
}

#[tauri::command]
fn new_pin() -> String {
    util::random_pin()
}

#[tauri::command]
fn stats(mgr: M) -> Value {
    use chrono::{Datelike, NaiveDate};
    let st = mgr.st.lock();
    let days: Vec<(NaiveDate, DayStat)> = st.stats.days.iter().filter_map(|(k, v)| NaiveDate::parse_from_str(k, "%Y-%m-%d").ok().map(|d| (d, v.clone()))).collect();
    drop(st);
    let today = chrono::Local::now().date_naive();
    let sum = |f: &dyn Fn(NaiveDate) -> bool| {
        let mut s = DayStat::default();
        for (d, v) in &days {
            if f(*d) {
                s.bytes += v.bytes;
                s.active_seconds += v.active_seconds;
                s.files += v.files;
                s.peak_bps = s.peak_bps.max(v.peak_bps);
            }
        }
        s
    };
    let card = |s: DayStat| {
        let avg = if s.active_seconds > 1.0 { s.bytes as f64 / s.active_seconds } else { 0.0 };
        let mut parts = vec![format!("{} file{}", s.files, if s.files == 1 { "" } else { "s" })];
        if avg > 0.0 {
            parts.push(format!("avg {}", fmt::speed(avg)));
        }
        if s.peak_bps > 0.0 {
            parts.push(format!("peak {}", fmt::speed(s.peak_bps)));
        }
        if s.active_seconds >= 1.0 {
            parts.push(format!("{} downloading", fmt::duration(s.active_seconds)));
        }
        json!({ "bytes": fmt::size(s.bytes), "sub": parts.join(" · ") })
    };
    let month_start = today.with_day(1).unwrap_or(today);
    let chart: Vec<Value> = (0..30)
        .map(|i| {
            let d = today - chrono::Duration::days(29 - i);
            let b = days.iter().find(|(x, _)| *x == d).map(|(_, v)| v.bytes).unwrap_or(0);
            json!({ "day": d.format("%b %d").to_string(), "bytes": b, "text": fmt::size(b) })
        })
        .collect();
    let mut months: Vec<NaiveDate> = days.iter().filter_map(|(d, _)| d.with_day(1)).collect();
    months.sort();
    months.dedup();
    months.reverse();
    let months: Vec<Value> = months
        .into_iter()
        .map(|m| {
            let s = sum(&|d| d.year() == m.year() && d.month() == m.month());
            let avg = if s.active_seconds > 1.0 { s.bytes as f64 / s.active_seconds } else { 0.0 };
            json!({ "month": m.format("%B %Y").to_string(), "bytes": fmt::size(s.bytes), "files": s.files,
                    "time": if s.active_seconds >= 1.0 { fmt::duration(s.active_seconds) } else { "—".into() },
                    "avg": if avg > 0.0 { fmt::speed(avg) } else { "—".into() } })
        })
        .collect();
    json!({
        "today": card(sum(&|d| d == today)),
        "week": card(sum(&|d| d > today - chrono::Duration::days(7) && d <= today)),
        "month": card(sum(&|d| d >= month_start && d <= today)),
        "monthTitle": today.format("%B %Y").to_string(),
        "all": card(sum(&|_| true)),
        "chart": chart,
        "months": months,
    })
}

#[tauri::command]
fn reset_stats(mgr: M) {
    mgr.st.lock().stats.days.clear();
    mgr.save();
}

#[tauri::command]
async fn yt_version() -> String {
    video::version().await
}

#[tauri::command]
async fn yt_update() -> String {
    video::update().await
}

#[tauri::command]
async fn firewall_status(mgr: M<'_>, kind: String) -> R<Option<bool>> {
    let s = mgr.settings();
    // Torrent port 0 = random each start: only an any-port rule (port 0 matches "Any") covers it.
    let port = if kind == "torrent" { s.torrent_port } else { s.remote_port };
    if kind != "torrent" && !(1024..=65535).contains(&port) {
        return Ok(None);
    }
    Ok(Some(
        tauri::async_runtime::spawn_blocking(move || {
            if kind == "torrent" {
                win::firewall_rule_exists(&format!("{TORRENT_RULE} (TCP)"), port) && win::firewall_rule_exists(&format!("{TORRENT_RULE} (UDP)"), port)
            } else {
                win::firewall_rule_exists(REMOTE_RULE, port)
            }
        })
        .await
        .unwrap_or(true),
    ))
}

#[tauri::command]
async fn firewall_allow(mgr: M<'_>, kind: String) -> R<bool> {
    let s = mgr.settings();
    tauri::async_runtime::spawn_blocking(move || {
        if kind == "torrent" {
            let tcp = format!("{TORRENT_RULE} (TCP)");
            let udp = format!("{TORRENT_RULE} (UDP)");
            Ok(win::allow_through_firewall(&[(&tcp, "TCP"), (&udp, "UDP")], s.torrent_port, "Lets other BitTorrent peers reach FluxDM"))
        } else {
            Ok(win::allow_through_firewall(&[(REMOTE_RULE, "TCP")], s.remote_port, "Lets your phone reach FluxDM's remote page on the home network"))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn magnet_toggle(mgr: M) -> R<Value> {
    let turning_on = !win::is_magnet_handler();
    let ok = {
        let mut st = mgr.st.lock();
        win::set_magnet_handler(turning_on, &mut st.settings.magnet_handler_backup)
    };
    if !ok {
        return Err("Couldn't change the magnet: link handler.".into());
    }
    mgr.save();
    let ours = win::is_magnet_handler();
    // Windows keeps a default the user picked in Settings above our registry entry — send them there. That works
    // both ways: once FluxDM was chosen there, giving magnet links back also has to happen there.
    let needs_settings = turning_on != ours;
    if needs_settings {
        win::open_default_apps();
    }
    Ok(json!({ "ours": ours, "name": win::magnet_handler_name(), "needsSettings": needs_settings, "turningOn": turning_on }))
}

#[tauri::command]
fn open_extension_folder() -> R<()> {
    let (_, xpi) = ext::export().map_err(|e| e.to_string())?;
    win::show_in_folder(&xpi.to_string_lossy());
    Ok(())
}

#[tauri::command]
fn extension_info() -> Value {
    let browsers: Vec<Value> =
        ext::browsers().into_iter().map(|(name, exe, gecko)| json!({ "name": name, "exe": exe, "gecko": gecko })).collect();
    json!({ "signed": ext::is_signed(), "browsers": browsers })
}

#[tauri::command]
fn install_extension(exe: String) -> R<()> {
    ext::install_in(&exe).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_data_folder() {
    win::open(&store::data_dir().to_string_lossy());
}

#[tauri::command]
fn pair_phone(mgr: M) -> R<()> {
    let s = mgr.settings();
    if !api::remote_running() || !s.remote_enabled {
        return Err("Tick “Control FluxDM from my phone”, click Save, then Pair phone.".into());
    }
    win::open(&format!("http://127.0.0.1:{}/qr", s.remote_port));
    Ok(())
}

#[tauri::command]
fn bookmarklets(mgr: M, port: Option<i32>) -> Value {
    let s = mgr.settings();
    let host = format!("http://127.0.0.1:{}", port.unwrap_or(s.local_api_port));
    let exts: Vec<&str> = s.clipboard_extensions.split(' ').filter(|x| !x.is_empty()).map(|x| x.trim_start_matches('.')).collect();
    json!({
        "send": format!("javascript:void(window.open('{host}/add?url='+encodeURIComponent(location.href)+'&ref='+encodeURIComponent(document.referrer),'fluxdm','width=360,height=140'))"),
        "grab": format!("javascript:(()=>{{const e=/\\.({})([?#]|$)/i;const l=[...new Set([...document.links].map(a=>a.href).filter(h=>e.test(h.split('?')[0])||e.test(h)))];if(!l.length){{alert('FluxDM: no downloadable links on this page');return}}window.open('{host}/batch?urls='+encodeURIComponent(l.join('\\n'))+'&ref='+encodeURIComponent(location.href),'fluxdm','width=360,height=140')}})()", exts.join("|")),
    })
}

#[tauri::command]
async fn check_update(mgr: M<'_>, repo: Option<String>) -> R<Option<update::Release>> {
    update::check(&repo.unwrap_or_else(|| mgr.settings().update_repo)).await
}

#[tauri::command]
async fn install_update(app: AppHandle, release: update::Release) -> R<()> {
    let h = app.clone();
    let exe = update::install(&release, move |p| {
        let _ = h.emit("update-progress", p);
    })
    .await?;
    // Close everything cleanly, then hand over to the new version.
    let mgr = app.state::<Arc<Manager>>().inner().clone();
    let resume = mgr.update_resume();
    store::write("update-resume.json", &resume).map_err(|e| format!("Couldn't save downloads to resume after the update: {e}"))?;
    mgr.exiting.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(15), mgr.pause_all()).await;
    mgr.save();
    // The new copy waits for this process to be gone (see main), or it would hand itself over to us and quit.
    if let Err(e) = std::process::Command::new(exe).arg(format!("--updated={}", std::process::id())).spawn() {
        let _ = std::fs::remove_file(store::path("update-resume.json"));
        mgr.exiting.store(false, Ordering::SeqCst);
        mgr.resume_after_update(&resume);
        return Err(format!("Couldn't start the updated version: {e}. Your downloads have been resumed."));
    }
    let _ = tokio::time::timeout(Duration::from_secs(4), mgr.torrents.shutdown()).await;
    api::stop_local();
    api::stop_remote();
    app.exit(0);
    Ok(())
}

// ------------------------------------------------------------------ main

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wants_exit = args.iter().any(|a| a.eq_ignore_ascii_case("--exit"));
    let tray_only = args.iter().any(|a| a == "--tray");
    let updated = args.iter().any(|a| a == "--updated" || a.starts_with("--updated="));
    let first_url = args.iter().skip(1).find(|a| net::is_download_source(a)).cloned();
    // Started by the updater: the old copy may still be closing. While its single-instance window exists, this
    // copy would pass its arguments to it and quit, leaving no FluxDM running at all.
    if let Some(pid) = args.iter().find_map(|a| a.strip_prefix("--updated=")).and_then(|p| p.parse::<u32>().ok()) {
        win::wait_for_exit(pid, Duration::from_secs(20));
    }
    update::cleanup_old();
    // Crashes go to error.log in the data folder, like the .NET build's unhandled-exception handlers.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        util::log_error(&format!("panic: {info}"));
        default_hook(info);
    }));

    webview_guard::apply_env();
    let mgr = Manager::new();
    let img_mgr = mgr.clone();
    // A copy with its own data folder (FLUXDM_DATA, for testing) runs beside the real one.
    let isolated = std::env::var("FLUXDM_DATA").map(|v| !v.trim().is_empty()).unwrap_or(false);

    let mut builder = tauri::Builder::default();
    if !isolated {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a.eq_ignore_ascii_case("--exit")) {
                let h = app.clone();
                tauri::async_runtime::spawn(async move { exit_app(&h).await });
                return;
            }
            match args.iter().skip(1).find(|a| net::is_download_source(a)) {
                Some(u) => show_add(app, AddRequest { urls: u.clone(), ..Default::default() }),
                None => show_main(app),
            }
        }));
    }
    builder
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("img", move |_ctx, req, responder| {
            let mgr = img_mgr.clone();
            std::thread::spawn(move || {
                let resp = match covers::serve(&mgr, req.uri().path()) {
                    Some((b, mime)) => tauri::http::Response::builder().header("Content-Type", mime).header("Cache-Control", "max-age=86400").body(b),
                    None => tauri::http::Response::builder().status(404).body(Vec::new()),
                };
                if let Ok(r) = resp {
                    responder.respond(r);
                }
            });
        })
        .manage(mgr.clone())
        .setup(move |tapp| {
            let handle = tapp.handle().clone();
            if wants_exit {
                // "--exit" with nothing running: nothing to close.
                handle.exit(0);
                return Ok(());
            }
            mgr.load(updated);
            apply_window_theme(&handle, &mgr.settings().theme);
            {
                let h = handle.clone();
                mgr.set_ui(Box::new(move |event, payload| match event {
                    "toast" => show_toast(&h, payload),
                    "countdown" => {
                        show_main(&h);
                        let _ = h.emit_to("main", "countdown", payload);
                    }
                    "torrent-engine" => {
                        if !FIREWALL_OFFERED.swap(true, Ordering::SeqCst) {
                            let port = payload["port"].as_i64().unwrap_or(0) as i32;
                            let h2 = h.clone();
                            std::thread::spawn(move || {
                                let ok = win::firewall_rule_exists(&format!("{TORRENT_RULE} (TCP)"), port) && win::firewall_rule_exists(&format!("{TORRENT_RULE} (UDP)"), port);
                                if !ok {
                                    let _ = h2.emit_to("main", "offer-firewall", json!({ "kind": "torrent" }));
                                }
                            });
                        }
                    }
                    _ => {
                        let _ = h.emit(event, payload);
                    }
                }));
            }

            // Tray
            let menu = Menu::with_items(
                &handle,
                &[
                    &MenuItem::with_id(&handle, "open", "Open FluxDM", true, None::<&str>)?,
                    &MenuItem::with_id(&handle, "add", "Add URL…", true, None::<&str>)?,
                    &MenuItem::with_id(&handle, "pause", "Pause all", true, None::<&str>)?,
                    &MenuItem::with_id(&handle, "reload", "Reload window (if it looks blank)", true, None::<&str>)?,
                    &PredefinedMenuItem::separator(&handle)?,
                    &MenuItem::with_id(&handle, "quit", "Exit", true, None::<&str>)?,
                ],
            )?;
            TrayIconBuilder::with_id("main")
                .icon(tapp.default_window_icon().cloned().expect("icon"))
                .tooltip("FluxDM")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|h, ev| match ev.id().as_ref() {
                    "open" => show_main(h),
                    "add" => show_add(h, AddRequest::default()),
                    "pause" => {
                        let m = h.state::<Arc<Manager>>().inner().clone();
                        tauri::async_runtime::spawn(async move { m.pause_all().await });
                    }
                    "reload" => {
                        show_main(h);
                        if let Some(w) = h.get_webview_window("main") {
                            webview_guard::reload(&w);
                        }
                    }
                    "quit" => {
                        let h = h.clone();
                        tauri::async_runtime::spawn(async move { exit_app(&h).await });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        show_main(tray.app_handle());
                    }
                })
                .build(tapp)?;

            // Closing the main window keeps FluxDM in the tray (or exits, per settings).
            if let Some(w) = handle.get_webview_window("main") {
                let h = handle.clone();
                let m = mgr.clone();
                w.on_window_event(move |ev| {
                    if let WindowEvent::CloseRequested { api, .. } = ev {
                        api.prevent_close();
                        save_geometry(&h, &m);
                        if m.settings().close_to_tray {
                            if let Some(w) = h.get_webview_window("main") {
                                let _ = w.hide();
                            }
                            if !TRAY_HINT_SHOWN.swap(true, Ordering::SeqCst) {
                                m.toast("FluxDM is still running", "Downloads continue in the background. Right-click the tray icon to exit.", None);
                            }
                        } else {
                            let h = h.clone();
                            tauri::async_runtime::spawn(async move { exit_app(&h).await });
                        }
                    }
                });
                restore_geometry(&w, &mgr.settings());
                if !tray_only {
                    let _ = w.show();
                    webview_guard::touch();
                    webview_guard::repaint_nudge(&w);
                }
                webview_guard::spawn_watchdog(handle.clone(), "main");
            }

            // Heartbeat
            {
                let m = mgr.clone();
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let mut iv = tokio::time::interval(Duration::from_millis(500));
                    loop {
                        iv.tick().await;
                        if m.exiting.load(Ordering::Relaxed) {
                            continue;
                        }
                        m.tick();
                        update_chrome(&h, &m);
                    }
                });
            }
            {
                let m = mgr.clone();
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    apply_servers(&h, &m, true, true, true).await;
                    covers::refresh_all(&m);
                    let s = m.settings();
                    if s.auto_check_updates && !s.update_repo.is_empty() {
                        if let Ok(Some(rel)) = update::check(&s.update_repo).await {
                            let _ = h.emit_to("main", "update-available", &rel);
                        }
                    }
                });
            }
            start_clipboard_watch(handle.clone(), mgr.clone());
            if let Some(u) = first_url.clone() {
                show_add(&handle, AddRequest { urls: u, ..Default::default() });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            init, ready, start, pause, toggle, pause_all, remove, restart, clear_finished, set_items, add_mirrors, probe, refresh_link,
            details, open_item, show_item, copy_text, clipboard_url, sha256, set_global_limit, parse_speed, free_space, run_post_action,
            exit, toast_click, toast_hide, toast_ready, toast_show, open_add, dropped_torrent, add_init, add_take_links, classify, folder_for, video_probe, torrent_info, add_submit,
            webview_guard::ui_heartbeat, torrent_recheck, torrent_reannounce, torrent_move, torrent_add_trackers, torrent_copy, torrent_export, torrent_file, torrent_file_priority, icon_find, icon_choose,
            icon_mode, clean_title, queues, queue_save, queue_new, queue_delete, queue_run, queue_move, get_settings, save_settings,
            save_layout, new_pin, stats, reset_stats, yt_version, yt_update, firewall_status, firewall_allow, magnet_toggle,
            open_extension_folder, extension_info, install_extension, open_data_folder, pair_phone, bookmarklets, check_update, install_update
        ])
        .build(tauri::generate_context!())
        .expect("FluxDM failed to start")
        .run(|app, ev| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = ev {
                // Closing the last window isn't quitting; only an explicit exit is.
                let mgr = app.state::<Arc<Manager>>();
                if code.is_none() && !mgr.exiting.load(Ordering::Relaxed) {
                    api.prevent_exit();
                }
            }
        });
}
