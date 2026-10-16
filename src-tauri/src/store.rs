//! Where FluxDM keeps its files: %APPDATA%\FluxDM (the same folder the .NET version used), or a "data"
//! folder next to the exe when a portable.txt sits beside it, or FLUXDM_DATA for testing.

use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    if let Ok(env) = std::env::var("FLUXDM_DATA") {
        if !env.is_empty() {
            return PathBuf::from(env);
        }
    }
    if let Some(dir) = exe_dir() {
        if dir.join("portable.txt").exists() {
            return dir.join("data");
        }
    }
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(appdata).join("FluxDM")
});

pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

pub fn data_dir() -> PathBuf {
    let d = DATA_DIR.clone();
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn path(name: &str) -> PathBuf {
    data_dir().join(name)
}

/// Reads a JSON file; a file that can't be parsed is kept aside as *.corrupt and treated as missing.
pub fn read<T: DeserializeOwned>(name: &str) -> Option<T> {
    let p = path(name);
    let text = std::fs::read_to_string(&p).ok()?;
    let text = text.trim_start_matches('\u{feff}');
    match serde_json::from_str(text) {
        Ok(v) => Some(v),
        Err(e) => {
            let _ = std::fs::copy(&p, p.with_extension("json.corrupt"));
            crate::util::log_error(&format!("{name} couldn't be read: {e}"));
            None
        }
    }
}

/// Writes via a temp file + rename, so a crash mid-write never leaves a half-written file.
pub fn write<T: Serialize>(name: &str, value: &T) -> std::io::Result<()> {
    // Saves come from the heartbeat, download endings and UI commands at once; two writers sharing one temp
    // file could interleave and leave a corrupt list behind.
    static WRITING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _g = WRITING.lock();
    let p = path(name);
    let tmp = p.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &p)
}
