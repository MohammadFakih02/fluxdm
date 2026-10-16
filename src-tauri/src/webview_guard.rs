//! Keeps the WebView2 window from going blank (white or black).
//!
//! Known causes this works around: WebView2 stops rendering windows it thinks are hidden/covered, a window
//! created hidden paints nothing when first shown, and the page sometimes just stops drawing with no crash.
//!  - `apply_env` turns off the renderer backgrounding / occlusion tracking for every window of the app.
//!  - `repaint_nudge` resizes a freshly shown window by one pixel and back, which forces a layout + paint.
//!  - the watchdog listens for the page's own "I'm painting" beats (ui/guard.js); a visible window that stays
//!    silent gets nudged, and one that stays silent while focused for a long time gets reloaded.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewWindow};

/// Same list for every window, set before the first one is made (windows with different flags can't share a profile).
/// The disable-features list keeps wry's own defaults (msWebOOUI, msPdfOOUI, msSmartScreenProtection).
const BROWSER_ARGS: &str = "--disable-features=CalculateNativeWinOcclusion,msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-background-timer-throttling --disable-backgrounding-occluded-windows --disable-renderer-backgrounding";

static START: OnceLock<Instant> = OnceLock::new();
static LAST_BEAT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Call first thing in `main`, before any window exists.
pub fn apply_env() {
    let value = match std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
        Ok(v) if !v.trim().is_empty() && !v.contains("CalculateNativeWinOcclusion") => format!("{v} {BROWSER_ARGS}"),
        Ok(v) if !v.trim().is_empty() => v,
        _ => BROWSER_ARGS.to_string(),
    };
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", value);
    }
}

/// The page reports that it is drawing frames.
#[tauri::command]
pub fn ui_heartbeat() {
    touch();
}

/// Restart the "has it gone quiet?" clock (call when the window is shown).
pub fn touch() {
    LAST_BEAT.store(now_ms(), Ordering::Relaxed);
}

/// One-pixel resize and back, shortly after the window is shown.
pub fn repaint_nudge(w: &WebviewWindow) {
    touch();
    let w = w.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(350));
        if w.is_maximized().unwrap_or(false) || w.is_fullscreen().unwrap_or(false) {
            return;
        }
        if let Ok(sz) = w.inner_size() {
            let _ = w.set_size(tauri::PhysicalSize::new(sz.width + 1, sz.height));
            std::thread::sleep(Duration::from_millis(40));
            let _ = w.set_size(sz);
        }
    });
}

pub fn reload(w: &WebviewWindow) {
    let _ = w.eval("location.reload()");
    touch();
}

/// Watches the window labelled `label`.
pub fn spawn_watchdog(app: AppHandle, label: &'static str) {
    touch();
    std::thread::spawn(move || {
        let mut nudged_at = 0u64;
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let Some(w) = app.get_webview_window(label) else { continue };
            let showing = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
            if !showing {
                touch(); // a hidden window owes us nothing; the clock starts when it's shown again
                continue;
            }
            let now = now_ms();
            let quiet = now.saturating_sub(LAST_BEAT.load(Ordering::Relaxed));
            // Focus matters for the reload: a locked screen or a sleeping display also silences the page.
            if quiet > 90_000 && w.is_focused().unwrap_or(false) {
                reload(&w);
            } else if quiet > 15_000 && now.saturating_sub(nudged_at) > 15_000 {
                nudged_at = now;
                repaint_nudge(&w);
            }
        }
    });
}
