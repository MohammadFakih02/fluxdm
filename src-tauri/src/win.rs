//! Everything that talks to Windows directly: sparse files, keep-awake, sleep/shutdown, the Recycle Bin,
//! Explorer, the clipboard, the registry (autostart, magnet: handler), the firewall and file icons.

use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{channel, Sender};
use std::sync::{LazyLock, Mutex as StdMutex};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND};

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ------------------------------------------------------------------ files

/// A sparse file lets connections write far into the file immediately, without NTFS first zero-filling
/// gigabytes in front of them.
pub fn set_sparse(f: &std::fs::File) {
    use windows::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows::Win32::System::IO::DeviceIoControl;
    unsafe {
        let mut ret = 0u32;
        let _ = DeviceIoControl(HANDLE(f.as_raw_handle() as _), FSCTL_SET_SPARSE, None, 0, None, 0, Some(&mut ret), None);
    }
}

/// Free bytes on the drive holding `folder` (None for network paths or when Windows can't tell).
pub fn free_space(folder: &str) -> Option<i64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    if folder.starts_with("\\\\") {
        return None;
    }
    let mut p = Path::new(folder).to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let w = wide(&p.to_string_lossy());
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), None, None).ok()? };
    Some(free as i64)
}

pub fn open(path: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn show_in_folder(path: &str) {
    let p = Path::new(path);
    if p.exists() {
        let _ = Command::new("explorer.exe").raw_arg(format!("/select,\"{path}\"")).spawn();
    } else if let Some(dir) = p.parent().filter(|d| d.is_dir()) {
        let _ = Command::new("explorer.exe").raw_arg(format!("\"{}\"", dir.display())).spawn();
    }
}

/// Sends a file — or a folder, e.g. a multi-file torrent — to the Recycle Bin.
pub fn recycle(path: &str) {
    use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
    if !Path::new(path).exists() {
        return;
    }
    // pFrom is a double-null-terminated list.
    let mut from: Vec<u16> = path.encode_utf16().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    unsafe {
        let _ = SHFileOperationW(&mut op);
    }
}

/// Waits (up to `max`) for another process to end. Returns at once if it is already gone.
pub fn wait_for_exit(pid: u32, max: std::time::Duration) {
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(h, max.as_millis().min(u32::MAX as u128) as u32);
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    }
}

// ------------------------------------------------------------------ power

static AWAKE: LazyLock<StdMutex<Sender<bool>>> = LazyLock::new(|| {
    // SetThreadExecutionState applies to the calling thread, so one long-lived thread owns it.
    let (tx, rx) = channel::<bool>();
    std::thread::spawn(move || {
        use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
        while let Ok(on) = rx.recv() {
            unsafe {
                SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
            }
        }
    });
    StdMutex::new(tx)
});

pub fn keep_awake(on: bool) {
    let _ = AWAKE.lock().unwrap().send(on);
}

pub fn run_post_action(action: crate::model::PostAction, force: bool) {
    use crate::model::PostAction as P;
    #[link(name = "powrprof")]
    extern "system" {
        fn SetSuspendState(hibernate: u8, force: u8, disable_wake: u8) -> u8;
    }
    let f = if force { " /f" } else { "" };
    match action {
        P::Sleep => unsafe {
            SetSuspendState(0, force as u8, 0);
        },
        P::Hibernate => unsafe {
            SetSuspendState(1, force as u8, 0);
        },
        P::Shutdown => {
            let _ = Command::new("shutdown.exe").raw_arg(format!("/s /t 0{f}")).creation_flags(CREATE_NO_WINDOW).spawn();
        }
        P::Restart => {
            let _ = Command::new("shutdown.exe").raw_arg(format!("/r /t 0{f}")).creation_flags(CREATE_NO_WINDOW).spawn();
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ clipboard

pub fn clipboard_sequence() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

pub fn clipboard_text() -> Option<String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard};
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        IsClipboardFormatAvailable(CF_UNICODETEXT).ok()?;
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return None;
        }
        let out = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut len = 0;
            while *p.add(len) != 0 && len < 1 << 22 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        out
    }
}

pub fn set_clipboard_text(text: &str) -> bool {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let w = wide(text);
    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let ok = (|| {
            EmptyClipboard().ok()?;
            let g = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2).ok()?;
            let p = GlobalLock(g) as *mut u16;
            if p.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
            let _ = GlobalUnlock(g);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(g.0))).ok()?;
            Some(())
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}

// ------------------------------------------------------------------ registry

fn exe() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The window that has the keyboard focus right now (0 if none).
pub fn foreground() -> isize {
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize }
}

/// Gives the foreground back to `prev` if `own` grabbed it (a toast must never tab a game out).
pub fn give_back_foreground(own: isize, prev: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    if prev == 0 || prev == own {
        return;
    }
    unsafe {
        if GetForegroundWindow().0 as isize == own {
            let _ = SetForegroundWindow(HWND(prev as *mut _));
        }
    }
}

pub fn starts_with_windows() -> bool {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    hkcu.open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>("FluxDM"))
        .map(|cmd| run_entry_is_this_exe(&cmd, &exe()))
        .unwrap_or(false)
}

/// The same file, however it is spelled: an update can start FluxDM through a short (8.3) or differently
/// cased path, and a text comparison then wrongly reported "off" and the setting was lost.
fn run_entry_is_this_exe(cmd: &str, exe: &str) -> bool {
    let target = cmd.trim().strip_prefix('"').and_then(|r| r.split('"').next()).unwrap_or_else(|| cmd.split(" --").next().unwrap_or(cmd)).trim();
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    if same(target, exe) {
        return true;
    }
    match (std::fs::canonicalize(target), std::fs::canonicalize(exe)) {
        (Ok(a), Ok(b)) => same(&a.to_string_lossy(), &b.to_string_lossy()),
        _ => false,
    }
}

pub fn set_start_with_windows(on: bool) {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    if let Ok((k, _)) = hkcu.create_subkey(RUN_KEY) {
        if on {
            let _ = k.set_value("FluxDM", &format!("\"{}\" --tray", exe()));
        } else {
            let _ = k.delete_value("FluxDM");
        }
    }
}

// magnet: — an uncommon URL protocol has no UserChoice lock, so writing HKCU\Software\Classes\magnet
// directly takes effect immediately, with no admin rights. Switching back restores what was there before.
const MAGNET_KEY: &str = r"Software\Classes\magnet";

fn magnet_command() -> Option<String> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    hkcu.open_subkey(format!(r"{MAGNET_KEY}\shell\open\command")).ok()?.get_value::<String, _>("").ok()
}

#[link(name = "shlwapi")]
extern "system" {
    fn AssocQueryStringW(flags: u32, s: i32, assoc: *const u16, extra: *const u16, out: *mut u16, len: *mut u32) -> i32;
}

/// What Windows will really launch for magnet: — it can differ from the registry class, because a
/// default the user picked in Settings (e.g. at qBittorrent's install) outranks HKCU\Software\Classes.
fn magnet_resolved_command() -> Option<String> {
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // ASSOCSTR_COMMAND = 1
    let hr = unsafe { AssocQueryStringW(0, 1, w("magnet").as_ptr(), w("open").as_ptr(), buf.as_mut_ptr(), &mut len) };
    (hr == 0).then(|| String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1).min(buf.len())]))
}

pub fn is_magnet_handler() -> bool {
    magnet_resolved_command().or_else(magnet_command).map(|c| c.to_lowercase().contains(&exe().to_lowercase())).unwrap_or(false)
}

/// Opens Settings → Default apps filtered to FluxDM, where the user can pick it for MAGNET.
pub fn open_default_apps() {
    // `start` hands the URI to the shell; explorer.exe itself doesn't understand it and opens Documents.
    let _ = Command::new("cmd")
        .args(["/c", "start", "", r"ms-settings:defaultapps?registrypathtosearch=Software\FluxDM\Capabilities"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Lists FluxDM in Windows' "Default apps" so it can be chosen for magnet: there.
fn register_capabilities() {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let pid = r"Software\Classes\FluxDM.Url.Magnet";
    if let Ok((k, _)) = hkcu.create_subkey(pid) {
        let _ = k.set_value("", &"URL:Magnet link");
        let _ = k.set_value("URL Protocol", &"");
        if let Ok((c, _)) = k.create_subkey(r"shell\open\command") {
            let _ = c.set_value("", &format!("\"{}\" \"%1\"", exe()));
        }
    }
    if let Ok((k, _)) = hkcu.create_subkey(r"Software\FluxDM\Capabilities") {
        let _ = k.set_value("ApplicationName", &"FluxDM");
        let _ = k.set_value("ApplicationDescription", &"Download manager and torrent client");
    }
    if let Ok((k, _)) = hkcu.create_subkey(r"Software\FluxDM\Capabilities\UrlAssociations") {
        let _ = k.set_value("magnet", &"FluxDM.Url.Magnet");
    }
    if let Ok((k, _)) = hkcu.create_subkey(r"Software\RegisteredApplications") {
        let _ = k.set_value("FluxDM", &r"Software\FluxDM\Capabilities");
    }
}

pub fn magnet_handler_name() -> String {
    let Some(cmd) = magnet_resolved_command().or_else(magnet_command).filter(|c| !c.trim().is_empty()) else { return "(none)".into() };
    let exe = if let Some(rest) = cmd.trim().strip_prefix('"') { rest.split('"').next().unwrap_or("") } else { cmd.split(' ').next().unwrap_or("") };
    Path::new(exe).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "(unknown)".into())
}

/// Turns FluxDM's magnet handling on/off. `backup` keeps the previous command ("" = there was none).
pub fn set_magnet_handler(on: bool, backup: &mut Option<String>) -> bool {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    if on {
        if backup.is_none() {
            *backup = Some(magnet_command().unwrap_or_default());
        }
        register_capabilities();
        let Ok((root, _)) = hkcu.create_subkey(MAGNET_KEY) else { return false };
        let _ = root.set_value("", &"URL:Magnet link");
        let _ = root.set_value("URL Protocol", &"");
        if let Ok((icon, _)) = root.create_subkey("DefaultIcon") {
            let _ = icon.set_value("", &format!("\"{}\",0", exe()));
        }
        let Ok((cmd, _)) = root.create_subkey(r"shell\open\command") else { return false };
        cmd.set_value("", &format!("\"{}\" \"%1\"", exe())).is_ok()
    } else if let Some(prev) = backup.take() {
        if prev.is_empty() {
            let _ = hkcu.delete_subkey_all(MAGNET_KEY);
        } else if let Ok((cmd, _)) = hkcu.create_subkey(format!(r"{MAGNET_KEY}\shell\open\command")) {
            let _ = cmd.set_value("", &prev);
        }
        true
    } else {
        // No record of what was there before (e.g. the settings were reset): at least stop pointing at FluxDM.
        if magnet_command().is_some_and(|c| c.to_lowercase().contains(&exe().to_lowercase())) {
            let _ = hkcu.delete_subkey_all(MAGNET_KEY);
        }
        true
    }
}

// ------------------------------------------------------------------ firewall

/// Is there an inbound Windows Firewall rule by this name, for this exe and port?
pub fn firewall_rule_exists(rule: &str, port: i32) -> bool {
    let Ok(out) = Command::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={rule}"), "verbose"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return true; // can't tell — don't nag
    };
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    out.status.success()
        && text.contains(&exe().to_lowercase())
        && text.lines().any(|l| {
            l.trim_start().starts_with("localport:")
                && l.split(':').nth(1).map(|v| v.trim() == "any" || v.trim() == port.to_string()).unwrap_or(false)
        })
}

/// Adds (or replaces) inbound rules — Private networks only, this exe, this port — behind one UAC prompt.
pub fn allow_through_firewall(rules: &[(&str, &str)], port: i32, description: &str) -> bool {
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::System::Threading::WaitForSingleObject;
    let exe = exe();
    let cmd = rules
        .iter()
        .map(|(name, proto)| {
            // Port 0 (random torrent port) → a rule for this exe on any port.
            let ports = if port > 0 { format!(" localport={port}") } else { String::new() };
            format!(
                "netsh advfirewall firewall delete rule name=\"{name}\" >nul 2>&1 & netsh advfirewall firewall add rule name=\"{name}\" dir=in action=allow program=\"{exe}\" protocol={proto}{ports} profile=private description=\"{description}\""
            )
        })
        .collect::<Vec<_>>()
        .join(" & ");
    let verb = wide("runas");
    let file = wide("cmd.exe");
    let params = wide(&format!("/c {cmd}"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: 0,
        ..Default::default()
    };
    unsafe {
        if ShellExecuteExW(&mut info).is_err() {
            return false; // UAC declined
        }
        if !info.hProcess.is_invalid() {
            WaitForSingleObject(info.hProcess, 30_000);
            let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
        }
    }
    rules.iter().all(|(name, _)| firewall_rule_exists(name, port))
}

// ------------------------------------------------------------------ network

/// The address a phone on the same network can use (the interface that routes to the LAN).
pub fn lan_addresses() -> Vec<String> {
    let mut out = vec![];
    for probe in ["192.168.0.1:9", "10.0.0.1:9", "8.8.8.8:9"] {
        if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
            if s.connect(probe).is_ok() {
                if let Ok(a) = s.local_addr() {
                    let ip = a.ip();
                    if let std::net::IpAddr::V4(v4) = ip {
                        if v4.is_private() && !out.contains(&ip.to_string()) {
                            out.push(ip.to_string());
                        }
                    }
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ file icons

type IconReq = (String, std::sync::mpsc::Sender<Option<Vec<u8>>>);

static ICONS: LazyLock<StdMutex<Sender<IconReq>>> = LazyLock::new(|| {
    // Shell icon extraction wants a COM STA thread of its own.
    let (tx, rx) = channel::<IconReq>();
    std::thread::spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        // Windows' blank "unknown file" page: types whose app gives no icon get FluxDM's glyph instead.
        let blank = {
            let probe = std::env::temp_dir().join("fluxdm-icon-probe.fluxdm-unknown-type");
            let _ = std::fs::write(&probe, b"");
            let png = extract_icon(&probe.to_string_lossy());
            let _ = std::fs::remove_file(&probe);
            png
        };
        while let Ok((path, reply)) = rx.recv() {
            let _ = reply.send(extract_icon(&path).filter(|png| Some(png) != blank.as_ref()));
        }
    });
    StdMutex::new(tx)
});

/// The icon Windows shows for this file, as a 64 px PNG.
pub fn file_icon_png(path: &str) -> Option<Vec<u8>> {
    let (tx, rx) = channel();
    ICONS.lock().ok()?.send((path.to_string(), tx)).ok()?;
    rx.recv_timeout(std::time::Duration::from_secs(5)).ok().flatten()
}

fn extract_icon(path: &str) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
    use windows::Win32::UI::Shell::{IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY};
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let f: IShellItemImageFactory = windows::core::Interface::cast(&item).ok()?;
        let hbmp = f.GetImage(SIZE { cx: 64, cy: 64 }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let png = hbitmap_png(hbmp);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        png
    }
}

/// 32-bit HBITMAP (premultiplied BGRA) → PNG.
unsafe fn hbitmap_png(hbmp: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::*;
    let mut bm = BITMAP::default();
    if GetObjectW(HGDIOBJ(hbmp.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 || w > 512 || h > 512 {
        return None;
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    let dc = GetDC(None);
    let lines = GetDIBits(dc, hbmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
    ReleaseDC(None, dc);
    if lines == 0 {
        return None;
    }
    let any_alpha = px.chunks(4).any(|p| p[3] != 0);
    for p in px.chunks_mut(4) {
        let (b, g, r, a) = (p[0], p[1], p[2], p[3]);
        if !any_alpha {
            p.copy_from_slice(&[r, g, b, 255]);
        } else if a == 0 {
            p.copy_from_slice(&[0, 0, 0, 0]);
        } else {
            let un = |c: u8| ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
            p.copy_from_slice(&[un(r), un(g), un(b), a]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(&px).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod startup_tests {
    #[test]
    fn run_entry_matches_quoted_and_case_insensitive_paths() {
        assert!(super::run_entry_is_this_exe(r#""C:\Apps\FluxDM\FluxDM.exe" --tray"#, r"c:\apps\fluxdm\fluxdm.exe"));
        assert!(!super::run_entry_is_this_exe(r#""C:\Other\FluxDM.exe" --tray"#, r"C:\Apps\FluxDM\FluxDM.exe"));
        assert!(super::run_entry_is_this_exe(r"C:\Apps\FluxDM.exe --tray", r"C:\Apps\FluxDM.exe"));
    }
}
