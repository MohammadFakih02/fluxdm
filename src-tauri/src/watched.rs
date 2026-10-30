//! Polling keeps imports independent of platform filesystem notification quirks.
use crate::{manager::Manager, torrent};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::{Duration, Instant, SystemTime}};

#[derive(Default)]
pub struct Watcher {
    folder: String,
    seen: HashMap<PathBuf, Observation>,
    folder_error: bool,
}
struct Observation {
    len: u64,
    modified: Option<SystemTime>,
    since: Instant,
    warned: bool,
}
const QUIET: Duration = Duration::from_secs(4);

impl Watcher {
    pub async fn scan(&mut self, mgr: &Arc<Manager>, now: Instant, start: bool) {
        let folder = mgr.settings().torrent_watch_folder;
        if folder != self.folder {
            self.seen.clear();
            self.folder_error = false;
            self.folder = folder;
        }
        if self.folder.is_empty() { return; }
        let mut entries = match tokio::fs::read_dir(&self.folder).await {
            Ok(e) => { self.folder_error = false; e }
            Err(_) => {
                if !self.folder_error { mgr.warn_toast("Watched folder", "Cannot read the watched folder. Check the path and permissions."); }
                self.folder_error = true;
                return;
            }
        };
        let mut present = std::collections::HashSet::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("torrent")) { continue; }
            let Ok(meta) = tokio::fs::symlink_metadata(&path).await else { continue };
            if !meta.is_file() || meta.file_type().is_symlink() { continue; }
            present.insert(path.clone());
            let modified = meta.modified().ok();
            let state = self.seen.entry(path.clone()).or_insert(Observation {
                len: meta.len(), modified, since: now, warned: false,
            });
            if state.len != meta.len() || state.modified != modified {
                *state = Observation { len: meta.len(), modified, since: now, warned: false };
            }
            if now.saturating_duration_since(state.since) < QUIET || state.warned { continue; }
            if mgr.exiting.load(std::sync::atomic::Ordering::Relaxed) { return; }
            match import(mgr, &path, state.len, state.modified, start).await {
                Ok(()) => { self.seen.remove(&path); }
                Err(e) => {
                    state.warned = true;
                    mgr.warn_toast("Watched torrent not added", &e);
                }
            }
        }
        self.seen.retain(|p, _| present.contains(p));
    }
}

async fn import(mgr: &Arc<Manager>, path: &std::path::Path, len: u64, modified: Option<SystemTime>, start: bool) -> Result<(), String> {
    // Cap metadata reads so a misnamed payload cannot exhaust memory.
    if len == 0 || len > 64 * 1024 * 1024 { return Err("The .torrent file is empty or exceeds 64 MiB.".into()); }
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await.map_err(|_| "Cannot read the .torrent file.")?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes).await.map_err(|_| "Cannot read the .torrent file.")?;
    if bytes.len() as u64 != len { return Err("The .torrent file changed during import.".into()); }
    let meta = torrent::parse(&bytes).map_err(|_| "Invalid or incomplete .torrent file. Replace it to retry.")?;
    let current = tokio::fs::metadata(path).await.map_err(|_| "The .torrent file disappeared.")?;
    if current.len() != len || current.modified().ok() != modified {
        return Err("The .torrent file changed during import. Replace it to retry.".into());
    }
    tokio::fs::create_dir_all(torrent::dir()).await.map_err(|_| "Cannot create the torrent cache.")?;
    let cache = torrent::dir().join(format!("{}.torrent", meta.hash));
    let temp = cache.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    tokio::fs::write(&temp, bytes).await.map_err(|_| "Cannot save torrent metadata.")?;
    // A hash identifies immutable metadata; another import may already have cached it.
    let cache_valid = tokio::fs::read(&cache).await.ok()
        .and_then(|b| torrent::parse(&b).ok()).is_some_and(|m| m.hash == meta.hash);
    if cache_valid { let _ = tokio::fs::remove_file(&temp).await; }
    else if tokio::fs::rename(&temp, &cache).await.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err("Cannot publish torrent metadata.".into());
    }
    let mut item = mgr.create_torrent_item(&cache.to_string_lossy(), Some(&meta.name), None, None);
    item.info_hash = meta.hash;
    // Persist the cached source before archiving; a failed archive is safe to retry as a duplicate.
    mgr.add(vec![item], start);
    mgr.save_checked().map_err(|_| "Torrent added, but saving failed. Its source stays in the watched folder.")?;
    let current = tokio::fs::metadata(path).await.map_err(|_| "Torrent added, but the source disappeared.")?;
    if current.len() != len || current.modified().ok() != modified {
        return Err("Torrent added, but its source changed and was not archived.".into());
    }
    let added = path.parent().ok_or("Missing watched folder.")?.join("added");
    tokio::fs::create_dir_all(&added).await.map_err(|_| "Torrent added, but cannot create the added subfolder.")?;
    let name = path.file_name().ok_or("Missing torrent filename.")?;
    let mut target = added.join(name);
    if target.exists() { target = added.join(format!("{}-{}", uuid::Uuid::new_v4(), name.to_string_lossy())); }
    tokio::fs::rename(path, target).await.map_err(|_| "Torrent added, but cannot move its source into added. Check permissions.")?;
    Ok(())
}
