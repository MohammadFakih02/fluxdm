//! Updates FluxDM in place from GitHub Releases: the release must carry FluxDM.exe and FluxDM.exe.sha256.
//! The new exe is downloaded next to the running one and checked against its SHA-256; then the running exe
//! is renamed aside (Windows allows that), the new one takes its name, and it's started. The old copy is
//! deleted on the next start.

use crate::net;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub exe_url: String,
    pub sha_url: String,
    pub size: u64,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (a, b) = (parse_version(candidate), parse_version(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// The latest release, if it's newer than this build. Err with a readable reason.
pub async fn check(repo: &str) -> Result<Option<Release>, String> {
    let repo = repo.trim().trim_matches('/');
    if repo.split('/').count() != 2 {
        return Err("Set the update source (GitHub owner/repo) in Settings first.".into());
    }
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let r = net::HTTP
        .get(&url)
        .header("User-Agent", format!("FluxDM/{}", current_version()))
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| net::DlError::from(e).msg)?;
    if r.status().as_u16() == 404 {
        return Err("No releases found for that repository.".into());
    }
    if !r.status().is_success() {
        return Err(format!("GitHub answered {}", r.status()));
    }
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().unwrap_or("").to_string();
    if !is_newer(&tag, current_version()) {
        return Ok(None);
    }
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let find = |name: &str| assets.iter().find(|a| a["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))).cloned();
    let exe = find("FluxDM.exe").ok_or("The latest release has no FluxDM.exe")?;
    let sha = find("FluxDM.exe.sha256").ok_or("The latest release has no FluxDM.exe.sha256 checksum")?;
    Ok(Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        notes: v["body"].as_str().unwrap_or("").chars().take(2000).collect(),
        exe_url: exe["browser_download_url"].as_str().unwrap_or("").into(),
        sha_url: sha["browser_download_url"].as_str().unwrap_or("").into(),
        size: exe["size"].as_u64().unwrap_or(0),
    }))
}

/// How long the update download may go without receiving anything.
const STALL: Duration = Duration::from_secs(60);

fn exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

/// Downloads and verifies the new exe, swaps it in and returns its path (the caller starts it and exits).
pub async fn install(rel: &Release, progress: impl Fn(f64)) -> Result<PathBuf, String> {
    let exe = exe_path()?;
    let dir = exe.parent().ok_or("no folder")?.to_path_buf();
    let new = dir.join("FluxDM.new.exe");
    let old = dir.join("FluxDM.old.exe");

    let expected = net::HTTP.get(&rel.sha_url).timeout(Duration::from_secs(30)).send().await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
    let expected = expected.split_whitespace().next().unwrap_or("").to_lowercase();
    if expected.len() != 64 {
        return Err("The release's checksum file is not a SHA-256".into());
    }

    // A connection that goes silent must end in an error the window can show, not a "Downloading…" that never ends.
    let stalled = || "The download stopped (no data for 60 seconds) — check your connection and try again.".to_string();
    let mut resp = tokio::time::timeout(STALL, net::HTTP.get(&rel.exe_url).send()).await.map_err(|_| stalled())?.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Download failed ({})", resp.status()));
    }
    let total = resp.content_length().unwrap_or(rel.size) as f64;
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    while let Some(chunk) = tokio::time::timeout(STALL, resp.chunk()).await.map_err(|_| stalled())?.map_err(|e| e.to_string())? {
        buf.extend_from_slice(&chunk);
        if total > 0.0 {
            progress(buf.len() as f64 * 100.0 / total);
        }
    }
    let actual = hex::encode(sha2::Sha256::digest(&buf));
    if actual != expected {
        return Err("The downloaded update didn't match its checksum — not installed.".into());
    }
    std::fs::write(&new, &buf).map_err(|e| format!("Can't write next to FluxDM.exe ({e}). Is it installed in a protected folder?"))?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("Couldn't move the running exe aside: {e}"))?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("Couldn't put the new exe in place: {e}"));
    }
    Ok(exe)
}

/// Deletes the copy left behind by the last update.
pub fn cleanup_old() {
    if let Ok(exe) = exe_path() {
        if let Some(dir) = exe.parent() {
            let old = dir.join("FluxDM.old.exe");
            if old.exists() {
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        if std::fs::remove_file(&old).is_ok() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::is_newer("v2.1.0", "2.0.0"));
        assert!(super::is_newer("2.0.10", "2.0.9"));
        assert!(!super::is_newer("v2.0.0", "2.0.0"));
        assert!(!super::is_newer("1.9", "2.0.0"));
    }
}
