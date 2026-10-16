//! The browser extension travels inside FluxDM.exe. "Open extension folder" writes it out to the data
//! folder (unpacked, for Chrome/Edge/Brave) together with FluxDM-extension.xpi (for Waterfox/Firefox).
//! The .xpi is the Mozilla-signed one when `extension/signed/FluxDM-extension.xpi` existed at build time
//! (see build.rs); regular Firefox only installs signed add-ons.

use std::io::Write;
use std::path::PathBuf;

const FILES: &[(&str, &[u8])] = &[
    ("manifest.json", include_bytes!("../extension/manifest.json")),
    ("background.js", include_bytes!("../extension/background.js")),
    ("common.js", include_bytes!("../extension/common.js")),
    ("popup.html", include_bytes!("../extension/popup.html")),
    ("popup.css", include_bytes!("../extension/popup.css")),
    ("popup.js", include_bytes!("../extension/popup.js")),
    ("icons/16.png", include_bytes!("../extension/icons/16.png")),
    ("icons/32.png", include_bytes!("../extension/icons/32.png")),
    ("icons/48.png", include_bytes!("../extension/icons/48.png")),
    ("icons/96.png", include_bytes!("../extension/icons/96.png")),
    ("icons/128.png", include_bytes!("../extension/icons/128.png")),
];

#[cfg(signed_xpi)]
const SIGNED_XPI: Option<&[u8]> = Some(include_bytes!("../extension/signed/FluxDM-extension.xpi"));
#[cfg(not(signed_xpi))]
const SIGNED_XPI: Option<&[u8]> = None;

pub fn is_signed() -> bool {
    SIGNED_XPI.is_some()
}

/// The manifest without the Firefox-only parts that Chromium flags as errors: blocking webRequest (MV3
/// Chromium refuses it; background.js only uses it in Gecko), background.scripts and the gecko settings.
pub fn chromium_manifest(firefox: &[u8]) -> Vec<u8> {
    let Ok(mut m) = serde_json::from_slice::<serde_json::Value>(firefox) else { return firefox.to_vec() };
    if let Some(o) = m.as_object_mut() {
        o.remove("browser_specific_settings");
        if let Some(b) = o.get_mut("background").and_then(|b| b.as_object_mut()) {
            b.remove("scripts");
        }
        if let Some(p) = o.get_mut("permissions").and_then(|p| p.as_array_mut()) {
            p.retain(|x| x != "webRequestBlocking");
        }
    }
    serde_json::to_vec_pretty(&m).unwrap_or_else(|_| firefox.to_vec())
}

/// Writes the unpacked extension and the .xpi; returns (folder, xpi path).
pub fn export() -> std::io::Result<(PathBuf, PathBuf)> {
    let root = crate::store::data_dir();
    let dir = root.join("BrowserExtension");
    for (name, bytes) in FILES {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // The unpacked folder is for Chromium browsers, so it gets their manifest.
        let bytes = if *name == "manifest.json" { chromium_manifest(bytes) } else { bytes.to_vec() };
        if std::fs::read(&p).ok() != Some(bytes.clone()) {
            std::fs::write(&p, &bytes)?;
        }
    }
    let xpi = root.join("FluxDM-extension.xpi");
    if let Some(signed) = SIGNED_XPI {
        std::fs::write(&xpi, signed)?;
        return Ok((dir, xpi));
    }
    // Firefox-family browsers need a zip with forward-slash paths.
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&xpi)?);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in FILES {
        zip.start_file(*name, opts).map_err(std::io::Error::other)?;
        zip.write_all(bytes)?;
    }
    zip.finish().map_err(std::io::Error::other)?;
    Ok((dir, xpi))
}

/// Browsers registered in the Start Menu that can run the extension: (name, exe, gecko?).
pub fn browsers() -> Vec<(String, String, bool)> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    const GECKO: &[&str] = &["firefox.exe", "waterfox.exe", "librewolf.exe", "floorp.exe", "zen.exe"];
    const CHROMIUM: &[&str] = &["chrome.exe", "msedge.exe", "brave.exe", "vivaldi.exe", "opera.exe", "launcher.exe", "chromium.exe", "thorium.exe"];
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let Ok(k) = winreg::RegKey::predef(root).open_subkey(r"Software\Clients\StartMenuInternet") else { continue };
        for name in k.enum_keys().flatten() {
            let Ok(b) = k.open_subkey(&name) else { continue };
            let Ok(cmd) = b.open_subkey(r"shell\open\command").and_then(|c| c.get_value::<String, _>("")) else { continue };
            let exe = cmd.trim().trim_start_matches('"').split('"').next().unwrap_or("").trim().to_string();
            let file = std::path::Path::new(&exe).file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
            let gecko = GECKO.contains(&file.as_str());
            if !gecko && !CHROMIUM.contains(&file.as_str()) {
                continue;
            }
            if !std::path::Path::new(&exe).is_file() || out.iter().any(|(_, e, _)| e.eq_ignore_ascii_case(&exe)) {
                continue;
            }
            let label = b.get_value::<String, _>("").ok().filter(|l| !l.is_empty()).unwrap_or(name);
            out.push((label, exe, gecko));
        }
    }
    out
}

/// Firefox family: opens the .xpi in the browser, which shows its own "Add FluxDM Integration?" prompt.
/// Chromium family (no installs from files outside the store): opens its extensions page and the unpacked
/// folder, so it's Developer mode → Load unpacked → pick the folder.
pub fn install_in(exe: &str) -> std::io::Result<()> {
    let (dir, xpi) = export()?;
    let Some((_, _, gecko)) = browsers().into_iter().find(|(_, e, _)| e.eq_ignore_ascii_case(exe)) else {
        return Err(std::io::Error::other("That browser isn't installed any more."));
    };
    if gecko {
        std::process::Command::new(exe).arg(&xpi).spawn()?;
    } else {
        let file = exe.to_lowercase();
        let page = if file.ends_with("msedge.exe") {
            "edge://extensions"
        } else if file.contains("opera") {
            "opera://extensions"
        } else {
            "chrome://extensions"
        };
        std::process::Command::new(exe).arg(page).spawn()?;
        crate::win::open(&dir.to_string_lossy());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn chromium_manifest_drops_firefox_parts() {
        let m: serde_json::Value = serde_json::from_slice(&super::chromium_manifest(super::FILES[0].1)).unwrap();
        assert!(m.get("browser_specific_settings").is_none());
        assert!(m["background"].get("scripts").is_none());
        assert_eq!(m["background"]["service_worker"], "background.js");
        let perms = m["permissions"].as_array().unwrap();
        assert!(perms.iter().all(|p| p != "webRequestBlocking"));
        assert!(perms.iter().any(|p| p == "downloads"));
    }
}
