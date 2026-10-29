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
    if user_away() { return; }
    let w = w.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(350));
        if user_away() { return; }
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
}

/// Watches the window labelled `label`.
pub fn spawn_watchdog(app: AppHandle, label: &'static str) {
    std::thread::spawn(move || {
        #[cfg(windows)]
        presence::watch_display();
        let mut nudged_at = 0u64;
        let mut grace_at = now_ms();
        let mut suspended = true;
        let mut last_check = now_ms();
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let Some(w) = app.get_webview_window(label) else { continue };
            let showing = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
            if !showing || user_away() {
                suspended = true;
                continue;
            }
            let now = now_ms();
            if suspended || now.saturating_sub(last_check) > 10_000 {
                grace_at = now;
                suspended = false;
            }
            last_check = now;
            #[cfg(windows)]
            { grace_at = grace_at.max(presence::resumed_at()); }
            let quiet = quiet_ms(now, LAST_BEAT.load(Ordering::Relaxed), grace_at);
            // Focus matters for the reload: a locked screen or a sleeping display also silences the page.
            if quiet > 90_000 && w.is_focused().unwrap_or(false) {
                reload(&w);
                grace_at = now;
            } else if quiet > 15_000 && now.saturating_sub(nudged_at) > 15_000 {
                nudged_at = now;
                repaint_nudge(&w);
            }
        }
    });
}

fn quiet_ms(now: u64, beat: u64, grace: u64) -> u64 {
    now.saturating_sub(beat.max(grace))
}

fn user_away() -> bool {
    #[cfg(windows)]
    { presence::away() }
    #[cfg(not(windows))]
    { false }
}

#[cfg(windows)]
mod presence {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Power::{PowerSettingRegisterNotification, DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, POWERBROADCAST_SETTING};
    use windows::Win32::System::StationsAndDesktops::{CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, DESKTOP_SWITCHDESKTOP, UOI_NAME};
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::System::SystemServices::GUID_SESSION_DISPLAY_STATUS;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_POWERSETTINGCHANGE};

    static DISPLAY_OFF: AtomicBool = AtomicBool::new(false);
    static RESUMED_AT: AtomicU64 = AtomicU64::new(0);

    pub fn resumed_at() -> u64 { RESUMED_AT.load(Ordering::Relaxed) }

    unsafe extern "system" fn display_changed(_: *const core::ffi::c_void, kind: u32, data: *const core::ffi::c_void) -> u32 {
        if kind == PBT_POWERSETTINGCHANGE && !data.is_null() {
            let setting = unsafe { &*(data as *const POWERBROADCAST_SETTING) };
            if setting.PowerSetting == GUID_SESSION_DISPLAY_STATUS && setting.DataLength >= 4 {
                let state = unsafe { std::ptr::read_unaligned(setting.Data.as_ptr() as *const u32) };
                if DISPLAY_OFF.swap(state == 0, Ordering::Relaxed) && state != 0 {
                    RESUMED_AT.store(super::now_ms(), Ordering::Relaxed);
                }
            }
        }
        0
    }

    pub fn watch_display() {
        let params = Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS { Callback: Some(display_changed), Context: std::ptr::null_mut() }));
        let mut registration = std::ptr::null_mut();
        // The subscription and static callback live until the process exits.
        unsafe {
            let _ = PowerSettingRegisterNotification(&GUID_SESSION_DISPLAY_STATUS, DEVICE_NOTIFY_CALLBACK, HANDLE((params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast()), &mut registration);
        }
    }

    pub fn away() -> bool {
        if DISPLAY_OFF.load(Ordering::Relaxed) { return true; }
        unsafe {
            let mut input = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
            if GetLastInputInfo(&mut input).as_bool() && GetTickCount().wrapping_sub(input.dwTime) > 60_000 { return true; }
            // The secure lock desktop cannot be opened with switch access by this process.
            match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(DESKTOP_SWITCHDESKTOP.0 | DESKTOP_READOBJECTS.0)) {
                Ok(desktop) => {
                    let mut name = [0u16; 256];
                    let result = GetUserObjectInformationW(HANDLE(desktop.0), UOI_NAME, Some(name.as_mut_ptr().cast()), std::mem::size_of_val(&name) as u32, None);
                    let _ = CloseDesktop(desktop);
                    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
                    result.is_err() || !String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case("Default")
                }
                Err(_) => true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_grace_does_not_replace_real_heartbeat() {
        assert_eq!(quiet_ms(100_000, 0, 0), 100_000);
        assert_eq!(quiet_ms(100_000, 0, 99_000), 1_000);
        assert_eq!(quiet_ms(100_000, 99_500, 99_000), 500);
    }
}
