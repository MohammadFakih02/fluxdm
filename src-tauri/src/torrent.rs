//! The built-in BitTorrent client (librqbit): one session shared by all torrents, with DHT, UPnP port
//! mapping and fast resume. Turns magnet links / .torrent links / .torrent files into metadata (cached as
//! .torrent files in FluxDM's data folder) and keeps finished torrents seeding until the chosen ratio.

use crate::manager::{Manager, Run};
use crate::model::*;
use crate::net::{self, DlError, DlResult, ErrKind};
use librqbit::api::TorrentIdOrHash;
use librqbit::dht::{DhtPersistenceConfig, Id20};
use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, ListenerMode, ListenerOptions, Session, SessionOptions,
    SessionPersistenceConfig,
};

type ManagedTorrentHandle = Arc<librqbit::ManagedTorrent>;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// "No upload": the patched engine reads an upload limit of 1 B/s as "answer no requests" (vendor/PATCHES.md).
const NO_UPLOAD: Option<NonZeroU32> = NonZeroU32::new(1);

struct CachedMeta {
    len: u64,
    modified: Option<std::time::SystemTime>,
    meta: Arc<Meta>,
}

#[derive(Default)]
pub struct Service {
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
    live: Mutex<Option<Arc<Session>>>,
    seed_baseline: Mutex<HashMap<String, u64>>,
    port: Mutex<i32>,
    utp: Mutex<bool>,
    peer_limit: Mutex<usize>,
    /// "hash|addr" -> (when, fetched, uploaded, down B/s, up B/s), for per-peer speeds in the Peers tab.
    peer_rates: Mutex<HashMap<String, (Instant, u64, u64, f64, f64)>>,
    metadata: Mutex<HashMap<String, CachedMeta>>,
}

/// File priority levels as the item stores them (0 = don't download). Higher downloads first.
pub const PRIO_NORMAL: u8 = 2;

/// One engine level per file, or None while every wanted file is Normal.
fn levels(it: &Item, files: usize) -> Option<Vec<u8>> {
    if it.file_priorities.is_empty() {
        return None;
    }
    Some((0..files).map(|i| it.file_priorities.get(&(i as i32)).copied().unwrap_or(PRIO_NORMAL)).collect())
}

/// Live swarm numbers for the list: connected seeds, the trackers' seeder/leecher totals, availability.
fn swarm(h: &ManagedTorrentHandle, it: &mut Item) {
    it.connected_seeds = count_seeds(h);
    let answers = librqbit::tracker_status::get(&it.info_hash.to_lowercase());
    it.swarm_seeds = answers.values().filter_map(|a| a.seeds).max().unwrap_or(0) as i32;
    it.swarm_peers = answers.values().filter_map(|a| a.leeches).max().unwrap_or(0) as i32;
    it.availability = h.live().and_then(|l| l.availability());
}

/// Peers that have every piece.
fn count_seeds(h: &ManagedTorrentHandle) -> i32 {
    let total = h.metadata.load().as_ref().map(|m| m.lengths().total_pieces()).unwrap_or(0);
    if total == 0 {
        return 0;
    }
    h.live()
        .map(|l| {
            l.per_peer_stats_snapshot(librqbit::http_api_types::PeerStatsFilter::default())
                .peers
                .values()
                .filter(|p| p.pieces_have == Some(total))
                .count() as i32
        })
        .unwrap_or(0)
}

/// A tracker URL the way the engine's tracker code prints it (it parses them with `url`).
fn tracker_key(t: &str) -> String {
    url::Url::parse(t.trim()).map(|u| u.to_string()).unwrap_or_else(|_| t.trim().to_string())
}

/// A setting only the .NET build had in its settings file (kept in `extra`), or the default.
fn extra_bool(s: &Settings, key: &str, default: bool) -> bool {
    s.extra.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}
fn extra_num(s: &Settings, key: &str, default: i64) -> i64 {
    s.extra.get(key).and_then(|v| v.as_i64()).filter(|v| *v > 0).unwrap_or(default)
}

pub fn dir() -> PathBuf {
    crate::store::data_dir().join("torrents")
}

fn cache_path(hex: &str) -> PathBuf {
    dir().join(format!("{}.torrent", hex.to_lowercase()))
}

fn rate(kbps_bytes: i64) -> Option<NonZeroU32> {
    if kbps_bytes <= 0 {
        return None;
    }
    NonZeroU32::new(kbps_bytes.min(u32::MAX as i64) as u32)
}

fn anyhow_err(e: anyhow::Error) -> DlError {
    DlError::transient(format!("{e:#}"))
}

/// A parsed .torrent: name, files, info hash, trackers.
pub struct Meta {
    pub name: String,
    pub files: Vec<(String, u64)>,
    /// BEP 47 padding entries (same indexes as `files`): never written to disk.
    pub padding: Vec<bool>,
    pub multi: bool,
    pub hash: String,
    pub trackers: Vec<String>,
    /// Tier of each tracker above, from 1 (announce-list order, like the .NET build showed it).
    pub tiers: Vec<usize>,
    pub size: u64,
}

pub fn parse(bytes: &[u8]) -> DlResult<Meta> {
    let bad = || DlError::fatal("That isn't a valid .torrent file");
    let t = librqbit::torrent_from_bytes(bytes).map_err(|_| bad())?;
    let multi = t.info.data.files.is_some();
    let info = t.info.data.clone().validate().map_err(|_| bad())?;
    let name = info.name().map(|n| n.into_owned()).unwrap_or_else(|| "Torrent".into());
    // Every file, padding included, so indexes match the engine's (only_files / file_progress).
    let files: Vec<(String, u64)> = info.iter_file_details().map(|fd| (fd.filename.to_string(), fd.len)).collect();
    let padding: Vec<bool> = info.iter_file_details().map(|fd| fd.attrs().padding).collect();
    let mut trackers: Vec<String> = vec![];
    let mut tiers: Vec<usize> = vec![];
    for (n, tier) in t.announce_list.iter().enumerate() {
        for a in tier {
            let s = String::from_utf8_lossy(a.as_ref()).into_owned();
            if !trackers.contains(&s) {
                trackers.push(s);
                tiers.push(n + 1);
            }
        }
    }
    // A lone "announce" (no announce-list, or not in it) is a tier of its own, first.
    if let Some(a) = &t.announce {
        let s = String::from_utf8_lossy(a.as_ref()).into_owned();
        if !trackers.contains(&s) {
            trackers.insert(0, s);
            tiers.iter_mut().for_each(|t| *t += 1);
            tiers.insert(0, 1);
        }
    }
    let size = files.iter().map(|f| f.1).sum();
    Ok(Meta { name, multi, files, padding, hash: t.info_hash.as_string(), trackers, tiers, size })
}

pub fn source_hash(source: &str) -> Option<String> {
    if net::is_magnet(source) {
        librqbit::Magnet::parse(source).ok()?.as_id20().map(|h| h.as_string())
    } else {
        parse(&std::fs::read(source.trim().trim_matches('"')).ok()?).ok().map(|m| m.hash)
    }
}

/// Where a torrent's files go: a multi-file torrent gets its own folder (like qBittorrent/MonoTorrent did).
fn output_folder(it: &Item, meta: &Meta) -> String {
    if meta.multi {
        Path::new(&it.save_folder).join(net::sanitize(&meta.name)).to_string_lossy().into_owned()
    } else {
        it.save_folder.clone()
    }
}

impl Service {
    pub fn session_now(&self) -> Option<Arc<Session>> {
        self.live.lock().clone()
    }

    pub fn status_line(&self) -> String {
        let Some(s) = self.session_now() else { return String::new() };
        let dht = s.get_dht().map(|d| format!("DHT: {} nodes", d.stats().routing_table_size)).unwrap_or_else(|| "DHT off".into());
        let port = s.listen_addr().map(|a| a.port()).unwrap_or(*self.port.lock() as u16);
        format!("{dht} · port {port}{}", if *self.utp.lock() { " · TCP+µTP" } else { "" })
    }

    async fn session(&self, mgr: &Arc<Manager>) -> DlResult<Arc<Session>> {
        let mut g = self.session.lock().await;
        if let Some(s) = g.as_ref() {
            return Ok(s.clone());
        }
        std::fs::create_dir_all(dir())?;
        let (port, upnp, default_folder) = {
            let mut st = mgr.st.lock();
            if !(1024..=65535).contains(&st.settings.torrent_port) {
                // One fixed random port, so a router/firewall rule keeps working across restarts.
                let mut b = [0u8; 2];
                let _ = getrandom::fill(&mut b);
                st.settings.torrent_port = 20000 + (u16::from_le_bytes(b) as i32 % 40000);
                mgr.mark_dirty();
            }
            (st.settings.torrent_port, st.settings.torrent_use_upnp, st.settings.default_folder.clone())
        };
        let s = mgr.settings();
        // µTP reaches peers TCP can't (and is what most of the swarm speaks); the .NET build's switch still applies.
        let utp = extra_bool(&s, "TorrentEnableUtp", true);
        let peer_limit = extra_num(&s, "TorrentMaxConnectionsPerTorrent", 200) as usize;
        *self.port.lock() = port;
        *self.utp.lock() = utp;
        *self.peer_limit.lock() = peer_limit;
        let opts = SessionOptions {
            dht: Some(DhtSessionConfig {
                persistence: Some(DhtPersistenceConfig { config_filename: Some(dir().join("dht.json")), ..Default::default() }),
                ..Default::default()
            }),
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json { folder: Some(dir().join("session")) }),
            listen: Some(ListenerOptions {
                mode: if utp { ListenerMode::TcpAndUtp } else { ListenerMode::TcpOnly },
                listen_addr: (std::net::Ipv6Addr::UNSPECIFIED, port as u16).into(),
                enable_upnp_port_forwarding: upnp,
                ..Default::default()
            }),
            peer_limit: Some(peer_limit),
            client_name_and_version: Some(format!("FluxDM {}", env!("CARGO_PKG_VERSION"))),
            ..Default::default()
        };
        let session = Session::new_with_opts(PathBuf::from(default_folder), opts).await.map_err(anyhow_err)?;
        // Torrents the session remembered start by themselves; only the ones FluxDM is running should.
        let handles: Vec<ManagedTorrentHandle> = session.with_torrents(|it| it.map(|(_, h)| h.clone()).collect());
        for h in handles {
            if !h.is_paused() {
                let _ = session.pause(&h).await;
            }
        }
        *g = Some(session.clone());
        *self.live.lock() = Some(session.clone());
        mgr.emit("torrent-engine", json!({ "port": port }));
        Ok(session)
    }

    pub fn find(&self, hex: &str) -> Option<ManagedTorrentHandle> {
        let s = self.session_now()?;
        let id = Id20::from_str(&hex.to_lowercase()).ok()?;
        s.get(TorrentIdOrHash::Hash(id))
    }

    // ------------------------------------------------------------------ metadata

    /// The .torrent bytes for a magnet link (asks peers via DHT/trackers), a .torrent web link or file,
    /// cached as a .torrent file in the data folder.
    pub async fn resolve(&self, mgr: &Arc<Manager>, source: &str, headers: &str, status: impl Fn(&str)) -> DlResult<(Vec<u8>, String)> {
        let source = source.trim().trim_matches('"');
        std::fs::create_dir_all(dir())?;
        let bytes = if net::is_magnet(source) {
            let magnet = librqbit::Magnet::parse(source).map_err(|_| DlError::fatal("That magnet link isn't valid"))?;
            if let Some(h) = magnet.as_id20() {
                let cached = cache_path(&h.as_string());
                if let Ok(b) = std::fs::read(&cached) {
                    return Ok((b, cached.to_string_lossy().into_owned()));
                }
            }
            status("getting the torrent's info from peers…");
            let session = self.session(mgr).await?;
            let resp = session
                .add_torrent(AddTorrent::from_url(source), Some(AddTorrentOptions { list_only: true, ..Default::default() }))
                .await
                .map_err(anyhow_err)?;
            match resp {
                AddTorrentResponse::ListOnly(r) => r.torrent_bytes.to_vec(),
                AddTorrentResponse::AlreadyManaged(_, h) | AddTorrentResponse::Added(_, h) => {
                    h.with_metadata(|m| m.torrent_bytes.to_vec()).map_err(|_| DlError::transient("no metadata yet"))?
                }
            }
        } else if net::is_http_url(source) {
            status("downloading the .torrent file…");
            let ua = mgr.settings().user_agent;
            let fut = async {
                let resp = net::get_scoped(source, headers, &ua).await?;
                net::ensure_ok(&resp)?;
                Ok::<_, DlError>(resp.bytes().await?.to_vec())
            };
            tokio::time::timeout(Duration::from_secs(60), fut).await.map_err(|_| DlError::timeout("The .torrent link didn't answer"))??
        } else if Path::new(source).is_file() {
            std::fs::read(source)?
        } else {
            return Err(DlError::fatal("The .torrent file no longer exists"));
        };
        let meta = parse(&bytes)?;
        let path = cache_path(&meta.hash);
        if !path.exists() {
            std::fs::write(&path, &bytes)?;
        }
        Ok((bytes, path.to_string_lossy().into_owned()))
    }

    /// What the Add window shows about a torrent.
    pub async fn info(&self, mgr: &Arc<Manager>, source: &str, headers: &str) -> DlResult<Value> {
        let (bytes, cached) = self.resolve(mgr, source, headers, |_| {}).await?;
        let m = parse(&bytes)?;
        Ok(json!({
            "name": m.name,
            "size": m.size,
            "files": m.files.iter().enumerate().map(|(i, (p, l))| json!({ "index": i, "path": p, "length": l })).collect::<Vec<_>>(),
            "infoHash": m.hash,
            "cachedFile": cached,
        }))
    }

    // ------------------------------------------------------------------ adding / removing

    #[allow(clippy::too_many_arguments)]
    async fn add(&self, mgr: &Arc<Manager>, session: &Arc<Session>, bytes: Vec<u8>, meta: &Meta, output: &str, it: &Item, down_bps: i64, paused: bool) -> DlResult<ManagedTorrentHandle> {
        if !mgr.claim_torrent_hash(&it.id, &meta.hash) { return Err(DlError::cancelled()); }
        let skipped: HashSet<i32> = it.skipped_files.iter().copied().collect();
        let only: Vec<usize> = (0..meta.files.len()).filter(|i| !skipped.contains(&(*i as i32))).collect();
        let up = self.upload_limit(mgr, it);
        if let Some(h) = self.find(&meta.hash) {
            // Already in the engine: change its limits in place (re-adding would re-verify the data).
            h.set_ratelimits(LimitsConfig { download_bps: rate(down_bps), upload_bps: up });
            let want: HashSet<usize> = only.iter().copied().collect();
            let have: HashSet<usize> = h.only_files().map(|v| v.into_iter().collect()).unwrap_or_else(|| (0..meta.files.len()).collect());
            if want != have {
                let _ = session.update_only_files(&h, &want).await;
            }
            if let Some(l) = levels(it, meta.files.len()) {
                let _ = h.set_file_priorities(l);
            }
            return Ok(h);
        }
        let opts = AddTorrentOptions {
            paused,
            only_files: if only.len() == meta.files.len() { None } else { Some(only) },
            overwrite: true,
            output_folder: Some(output.to_string()),
            ratelimits: LimitsConfig { download_bps: rate(down_bps), upload_bps: up },
            trackers: if it.extra_trackers.is_empty() { None } else { Some(it.extra_trackers.clone()) },
            peer_limit: Some(*self.peer_limit.lock()).filter(|n| *n > 0),
            ..Default::default()
        };
        let resp = session.add_torrent(AddTorrent::from_bytes(bytes), Some(opts)).await.map_err(anyhow_err)?;
        let h = resp.into_handle().ok_or_else(|| DlError::transient("the torrent engine didn't take the torrent"))?;
        if let Some(l) = levels(it, meta.files.len()) {
            let _ = h.set_file_priorities(l);
        }
        Ok(h)
    }

    /// Takes a torrent out of the engine. Unfinished downloads also lose their partial data.
    pub async fn remove(&self, mgr: &Arc<Manager>, it: &Item, delete_data: bool) {
        self.metadata.lock().remove(&it.torrent_file);
        let _ = mgr;
        if let (Some(s), Some(h)) = (self.session_now(), self.find(&it.info_hash)) {
            let _ = s.delete(TorrentIdOrHash::Hash(h.info_hash()), delete_data).await;
            return;
        }
        if delete_data {
            // Not loaded this session: delete exactly the files the torrent lists, never anything else.
            if let Ok(bytes) = std::fs::read(&it.torrent_file) {
                if let Ok(meta) = parse(&bytes) {
                    let out = output_folder(it, &meta);
                    for (p, _) in &meta.files {
                        let _ = std::fs::remove_file(Path::new(&out).join(p));
                    }
                    if meta.multi {
                        remove_empty_dirs(Path::new(&out));
                    }
                }
            }
        }
    }

    /// "Resume" on a finished torrent: share it again until the seed ratio (Pause stops it; after a restart the
    /// engine starts with everything paused). Only when every wanted file is still there at its full size, so a
    /// torrent whose files were deleted or moved can't quietly start downloading again under "Complete".
    pub async fn resume_seeding(&self, mgr: &Arc<Manager>, id: &str) {
        let Some(it) = mgr.item(id) else { return };
        let note = |n: String| {
            mgr.with_item(id, |x| x.note = n);
        };
        let s = mgr.settings();
        let target = s.torrent_seed_ratio;
        if target <= 0.0 {
            return note("not shared (Settings → Torrents → keep sharing until ratio is 0)".into());
        }
        if it.seeding_limit_kbps < 0 || s.torrent_seeding_limit_kbps < 0 {
            return note("not shared (seeding limit is No upload)".into());
        }
        if it.ratio() >= target {
            return note(format!("already shared {:.2}× (target {target})", it.ratio()));
        }
        let Some((bytes, meta)) = std::fs::read(&it.torrent_file).ok().and_then(|b| parse(&b).ok().map(|m| (b, m))) else {
            return note("can't share: this torrent's info file is missing".into());
        };
        let folder = output_folder(&it, &meta);
        let skipped: HashSet<i32> = it.skipped_files.iter().copied().collect();
        let missing = meta
            .files
            .iter()
            .enumerate()
            .filter(|(i, (_, len))| *len > 0 && !skipped.contains(&(*i as i32)) && !meta.padding.get(*i).copied().unwrap_or(false))
            .any(|(_, (p, len))| std::fs::metadata(Path::new(&folder).join(p)).map(|m| m.len() != *len).unwrap_or(true));
        if missing {
            return note("can't share: some of its files were moved or deleted (Force recheck to download them again)".into());
        }
        let r: DlResult<()> = async {
            let session = self.session(mgr).await?;
            let h = match self.find(&meta.hash) {
                Some(h) => h,
                None => self.add(mgr, &session, bytes, &meta, &folder, &it, 0, false).await?,
            };
            if h.is_paused() {
                h.set_ratelimits(LimitsConfig { download_bps: None, upload_bps: self.upload_limit(mgr, &it) });
                session.unpause(&h).await.map_err(anyhow_err)?;
            }
            Ok(())
        }
        .await;
        note(match r {
            Ok(()) => "seeding".into(),
            Err(e) => format!("couldn't start sharing: {}", e.msg),
        });
    }

    pub async fn stop_seeding(&self, mgr: &Arc<Manager>, id: &str) {
        let hash = mgr.with_item(id, |it| it.info_hash.clone()).unwrap_or_default();
        if let (Some(s), Some(h)) = (self.session_now(), self.find(&hash)) {
            if !h.is_paused() {
                let _ = s.pause(&h).await;
            }
        }
        mgr.with_item(id, |it| { it.note.clear(); it.seeding = false; it.upload_speed = 0.0; it.connected_peers = 0; });
    }

    /// Re-reads every piece already on disk and keeps only what verifies ("Force recheck"). The caller pauses
    /// the item first. Returns the verified share of the wanted files, 0–100.
    pub async fn recheck(&self, mgr: &Arc<Manager>, id: &str) -> DlResult<f64> {
        let it = mgr.item(id).ok_or_else(|| DlError::fatal("gone"))?;
        let bytes = std::fs::read(&it.torrent_file).map_err(|_| DlError::fatal("This torrent's info isn't known yet — start it once first"))?;
        let meta = parse(&bytes)?;
        let session = self.session(mgr).await?;
        if let Some(h) = self.find(&meta.hash) {
            let _ = session.delete(TorrentIdOrHash::Hash(h.info_hash()), false).await;
        }
        let h = self.add(mgr, &session, bytes, &meta, &output_folder(&it, &meta), &it, 0, true).await?;
        loop {
            let st = h.stats();
            if matches!(st.state, librqbit::TorrentStatsState::Paused | librqbit::TorrentStatsState::Live | librqbit::TorrentStatsState::Error) {
                let pct = if st.total_bytes > 0 { st.progress_bytes as f64 * 100.0 / st.total_bytes as f64 } else { 0.0 };
                return Ok(pct);
            }
            let pct = if st.total_bytes > 0 { st.progress_bytes as f64 * 100.0 / st.total_bytes as f64 } else { 0.0 };
            mgr.with_item(id, |it| it.verify_progress = pct);
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Moves the downloaded data (finished or not) to another folder. The caller pauses the item first.
    pub async fn move_data(&self, mgr: &Arc<Manager>, id: &str, new_folder: &str) -> DlResult<()> {
        let it = mgr.item(id).ok_or_else(|| DlError::fatal("gone"))?;
        let paths = move_targets(&it, new_folder);
        check_move_targets(&paths)?;
        if let (Some(s), Some(h)) = (self.session_now(), self.find(&it.info_hash)) {
            let _ = s.delete(TorrentIdOrHash::Hash(h.info_hash()), false).await;
        }
        move_paths(&paths)?;
        Ok(())
    }

    /// A magnet link for the item (with its trackers), e.g. to share it.
    pub fn magnet_for(it: &Item) -> String {
        let Ok(bytes) = std::fs::read(&it.torrent_file) else {
            return if net::is_magnet(&it.url) { it.url.clone() } else { String::new() };
        };
        let Ok(m) = parse(&bytes) else { return String::new() };
        let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        let mut out = format!("magnet:?xt=urn:btih:{}&dn={}", m.hash.to_lowercase(), enc(&m.name));
        let mut seen = HashSet::new();
        for t in m.trackers.iter().chain(it.extra_trackers.iter()) {
            if seen.insert(t.to_lowercase()) {
                out.push_str(&format!("&tr={}", enc(t)));
            }
        }
        out
    }

    /// Live details for the Trackers / Peers / Content tabs.
    pub fn details(&self, it: &Item) -> Value {
        let meta = self.cached_meta(&it.torrent_file);
        let h = self.find(&it.info_hash);
        let stats = h.as_ref().map(|h| h.stats());
        let skipped: HashSet<i32> = it.skipped_files.iter().copied().collect();
        let files: Vec<Value> = meta
            .as_ref()
            .map(|m| {
                m.files
                    .iter()
                    .enumerate()
                    .map(|(i, (p, len))| {
                        let have = stats.as_ref().and_then(|s| s.file_progress.get(i).copied()).unwrap_or(if it.status == DlStatus::Completed && !skipped.contains(&(i as i32)) { *len } else { 0 });
                        let pct = if *len > 0 { have as f64 * 100.0 / *len as f64 } else { 100.0 };
                        json!({
                            "index": i, "path": p, "sizeText": fmt::size(*len as i64),
                            "progress": (pct * 10.0).round() / 10.0,
                            "wanted": !skipped.contains(&(i as i32)),
                            "priority": if skipped.contains(&(i as i32)) { 0 } else { it.file_priorities.get(&(i as i32)).copied().unwrap_or(PRIO_NORMAL) },
                            "remaining": if skipped.contains(&(i as i32)) { String::new() } else { fmt::size(len.saturating_sub(have) as i64) },
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let running = h.as_ref().map(|h| !h.is_paused()).unwrap_or(false);
        let answers = librqbit::tracker_status::get(&it.info_hash.to_lowercase());
        let now = Instant::now();
        let tracker_row = |t: &str, tier: usize, mine: bool| {
            use librqbit::tracker_status::State;
            let a = answers.get(&tracker_key(t));
            let supported = t.starts_with("http") || t.starts_with("udp");
            let status = match (running, a.map(|a| &a.state)) {
                _ if !supported => "Not supported",
                (false, _) => "Stopped",
                (true, None | Some(State::NotContacted)) => "Not contacted yet",
                (true, Some(State::Updating)) => "Updating…",
                (true, Some(State::Working)) => "Working",
                (true, Some(State::Error)) => "Not working",
            };
            let num = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_default();
            let next = a
                .and_then(|a| a.next_announce)
                .filter(|_| running)
                .map(|t| t.saturating_duration_since(now).as_secs())
                .map(|s| if s >= 60 { format!("{}m {}s", s / 60, s % 60) } else { format!("{s}s") })
                .unwrap_or_default();
            json!({
                "url": t,
                "tier": tier,
                "status": status,
                "seeds": num(a.and_then(|a| a.seeds)),
                "leeches": num(a.and_then(|a| a.leeches)),
                "peers": a.filter(|a| a.last_ok.is_some()).map(|a| a.peers_received.to_string()).unwrap_or_default(),
                "message": a.map(|a| a.message.clone()).unwrap_or_default(),
                "next": next,
                "mine": mine,
            })
        };
        let mut trackers: Vec<Value> =
            meta.as_ref().map(|m| m.trackers.iter().zip(&m.tiers).map(|(t, tier)| tracker_row(t, *tier, false)).collect()).unwrap_or_default();
        // Trackers you added each get a tier of their own after the file's (as in the .NET build).
        let mut last = meta.as_ref().and_then(|m| m.tiers.iter().max().copied()).unwrap_or(0);
        for t in &it.extra_trackers {
            last += 1;
            trackers.push(tracker_row(t, last, true));
        }
        let mut peers: Vec<Value> = vec![];
        let mut seeds = 0;
        if let Some(live) = h.as_ref().and_then(|h| h.live()) {
            let total = h.as_ref().and_then(|h| h.metadata.load().as_ref().map(|m| m.lengths().total_pieces())).unwrap_or(0);
            let snap = live.per_peer_stats_snapshot(librqbit::http_api_types::PeerStatsFilter::default());
            let hash = it.info_hash.to_lowercase();
            let mut rates = self.peer_rates.lock();
            rates.retain(|_, v| now.duration_since(v.0) < Duration::from_secs(30));
            let mut list: Vec<_> = snap
                .peers
                .into_iter()
                .map(|(addr, p)| {
                    // Speeds from the byte counters between two looks, smoothed.
                    let (f, u) = (p.counters.fetched_bytes, p.counters.uploaded_bytes);
                    let key = format!("{hash}|{addr}");
                    let (down, up) = match rates.get(&key) {
                        Some(&(t, f0, u0, d0, u0s)) => {
                            let dt = now.duration_since(t).as_secs_f64();
                            if dt < 0.3 {
                                (d0, u0s)
                            } else {
                                let d = f.saturating_sub(f0) as f64 / dt;
                                let w = u.saturating_sub(u0) as f64 / dt;
                                (d0 * 0.4 + d * 0.6, u0s * 0.4 + w * 0.6)
                            }
                        }
                        None => (0.0, 0.0),
                    };
                    if rates.get(&key).map(|r| now.duration_since(r.0).as_secs_f64() >= 0.3).unwrap_or(true) {
                        rates.insert(key, (now, f, u, down, up));
                    }
                    (addr, p, down, up)
                })
                .collect();
            list.sort_by(|a, b| (b.2 + b.3).total_cmp(&(a.2 + a.3)).then(b.1.counters.fetched_bytes.cmp(&a.1.counters.fetched_bytes)));
            for (addr, p, down, up) in list.into_iter().take(200) {
                let kind = p.conn_kind.and_then(|k| serde_json::to_value(k).ok()).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                let seed = total > 0 && p.pieces_have == Some(total);
                seeds += seed as i32;
                let progress = match (p.pieces_have, total) {
                    (Some(n), t) if t > 0 => format!("{:.1}%", n as f64 * 100.0 / t as f64),
                    _ => String::new(),
                };
                let mut flags = vec![if p.counters.incoming_connections > 0 { "incoming" } else { "outgoing" }];
                if seed {
                    flags.push("seed");
                }
                if p.peer_interested {
                    flags.push("wants our data");
                }
                peers.push(json!({
                    "address": addr,
                    "state": p.state,
                    "kind": if kind == "utp" { "µTP".to_string() } else { kind.to_uppercase() },
                    "client": p.client_name.unwrap_or_default(),
                    "progress": progress,
                    "seed": seed,
                    "flags": flags.join(" · "),
                    "down": if down >= 1.0 { fmt::speed(down) } else { String::new() },
                    "up": if up >= 1.0 { fmt::speed(up) } else { String::new() },
                    "downloaded": if p.counters.fetched_bytes > 0 { fmt::size(p.counters.fetched_bytes as i64) } else { String::new() },
                    "uploaded": if p.counters.uploaded_bytes > 0 { fmt::size(p.counters.uploaded_bytes as i64) } else { String::new() },
                    "incoming": p.counters.incoming_connections > 0,
                    "pieces": p.counters.downloaded_and_checked_pieces,
                }));
            }
        }
        let summary = stats.as_ref().and_then(|s| s.live.as_ref()).map(|l| {
            let ps = &l.snapshot.peer_stats;
            let hp = librqbit::holepunch_stats::get();
            json!({ "live": ps.live, "seeds": seeds, "seen": ps.seen,
                    "holepunch": format!("{} asked · {} relayed for others · {} invited · {} got through", hp.rendezvous_sent, hp.relayed, hp.connects_received, hp.connected),
                    "holepunchActive": hp.rendezvous_sent + hp.relayed + hp.connects_received > 0,
                    "turnover": librqbit::turnover_stats::dropped(), "connecting": ps.connecting, "queued": ps.queued, "dead": ps.dead,
                    "down": fmt::speed(l.download_speed.mbps * 1048576.0), "up": fmt::speed(l.upload_speed.mbps * 1048576.0) })
        });
        json!({ "files": files, "trackers": trackers, "peers": peers, "live": summary, "hash": it.info_hash.to_lowercase() })
    }

    /// Turns one file of a torrent on or off (live if it's running).
    pub async fn set_file_wanted(&self, mgr: &Arc<Manager>, id: &str, index: i32, wanted: bool) {
        let Some(it) = mgr.item(id) else { return };
        let meta = std::fs::read(&it.torrent_file).ok().and_then(|b| parse(&b).ok());
        let mut skipped: Vec<i32> = it.skipped_files.clone();
        if wanted {
            skipped.retain(|i| *i != index);
        } else if !skipped.contains(&index) {
            skipped.push(index);
        }
        let total: i64 = meta.as_ref().map(|m| m.files.iter().enumerate().filter(|(i, _)| !skipped.contains(&(*i as i32))).map(|(_, f)| f.1 as i64).sum()).unwrap_or(it.total_size);
        let reopen = it.status == DlStatus::Completed && wanted;
        mgr.with_item(id, |it| {
            it.skipped_files = skipped.clone();
            it.total_size = total;
            if reopen {
                it.status = DlStatus::Paused;
            }
        });
        if reopen {
            mgr.start(id);
        } else if let (Some(s), Some(h), Some(m)) = (self.session_now(), self.find(&it.info_hash), meta) {
            let only: HashSet<usize> = (0..m.files.len()).filter(|i| !skipped.contains(&(*i as i32))).collect();
            let _ = s.update_only_files(&h, &only).await;
        }
        mgr.save();
    }

    /// Sets one file's priority: 0 = don't download, 1 Low, 2 Normal, 3 High, 4 Maximum.
    pub async fn set_file_priority(&self, mgr: &Arc<Manager>, id: &str, index: i32, level: u8) {
        let Some(it) = mgr.item(id) else { return };
        if level == 0 {
            return self.set_file_wanted(mgr, id, index, false).await;
        }
        mgr.with_item(id, |it| {
            if level == PRIO_NORMAL {
                it.file_priorities.remove(&index);
            } else {
                it.file_priorities.insert(index, level.min(4));
            }
        });
        if it.skipped_files.contains(&index) {
            self.set_file_wanted(mgr, id, index, true).await;
        }
        let Some(it) = mgr.item(id) else { return };
        let files = std::fs::read(&it.torrent_file).ok().and_then(|b| parse(&b).ok()).map(|m| m.files.len());
        if let (Some(h), Some(n)) = (self.find(&it.info_hash), files) {
            let l = levels(&it, n).unwrap_or_else(|| vec![PRIO_NORMAL; n]);
            let _ = h.set_file_priorities(l);
        }
        mgr.save();
    }

    // ------------------------------------------------------------------ heartbeat (every second)

    fn cached_meta(&self, path: &str) -> Option<Arc<Meta>> {
        let file = std::fs::metadata(path).ok()?;
        let stamp = (file.len(), file.modified().ok());
        let mut cache = self.metadata.lock();
        if let Some(entry) = cache.get(path) {
            if (entry.len, entry.modified) == stamp { return Some(entry.meta.clone()); }
        }
        let meta = Arc::new(parse(&std::fs::read(path).ok()?).ok()?);
        cache.insert(path.to_string(), CachedMeta { len: stamp.0, modified: stamp.1, meta: meta.clone() });
        Some(meta)
    }

    fn upload_limit(&self, mgr: &Manager, it: &Item) -> Option<NonZeroU32> {
        let st = mgr.st.lock();
        let finished = it.status == DlStatus::Completed;
        let count = st.items.iter().filter(|i| i.is_torrent && (i.status == DlStatus::Completed) == finished
            && (i.id == it.id || self.find(&i.info_hash).is_some_and(|h| !h.is_paused()))).count();
        let (own, global) = if finished { (it.seeding_limit_kbps, st.settings.torrent_seeding_limit_kbps) }
            else { (it.upload_limit_kbps, st.settings.torrent_upload_limit_kbps) };
        class_upload_limit(own, global, count)
    }

    pub async fn tick(&self, mgr: &Arc<Manager>) {
        let Some(session) = self.session_now() else { return };
        let s = mgr.settings();
        session.ratelimits.set_download_bps(None);
        session.ratelimits.set_upload_bps(None);
        let items: Vec<Item> = mgr.st.lock().items.iter().filter(|i| i.is_torrent && !i.info_hash.is_empty()).cloned().collect();

        // Each torrent uses its download share and the upload budget for its own phase.
        for it in &items {
            let Some(h) = self.find(&it.info_hash) else { continue };
            let finished = it.status == DlStatus::Completed;
            h.set_sequential(it.sequential);
            h.set_ratelimits(LimitsConfig {
                download_bps: if finished { None } else { rate(own_down_limit(mgr, it)) },
                upload_bps: self.upload_limit(mgr, it),
            });
        }

        let mut seeding = HashSet::new();
        for it in items.iter().filter(|i| i.status == DlStatus::Completed && !mgr.is_running(&i.id)) {
            mgr.with_item(&it.id, |x| { x.seeding = false; x.upload_speed = 0.0; x.connected_peers = 0; });
            let Some(h) = self.find(&it.info_hash) else { continue };
            if h.is_paused() {
                continue;
            }
            // Shared from disk but not all there (files changed since it finished): stop rather than download
            // behind a "Complete" row. Force recheck is the way to fetch what's missing.
            let now = h.stats();
            if matches!(now.state, librqbit::TorrentStatsState::Live) && !now.finished {
                let _ = session.pause(&h).await;
                mgr.with_item(&it.id, |x| x.note = "stopped sharing: some data on disk doesn't match (Force recheck downloads it again)".into());
                continue;
            }
            let hex = it.info_hash.to_lowercase();
            seeding.insert(hex.clone());
            let sent = h.stats().uploaded_bytes;
            let delta = {
                let mut b = self.seed_baseline.lock();
                let base = *b.get(&hex).unwrap_or(&sent);
                b.insert(hex.clone(), sent);
                sent.saturating_sub(base) as i64
            };
            let target = s.torrent_seed_ratio;
            let (ratio, peers) = mgr
                .with_item(&it.id, |x| {
                    if delta > 0 {
                        x.total_uploaded += delta;
                        x.last_activity_at = Some(chrono::Local::now());
                    }
                    x.active_seconds += 1.0;
                    swarm(&h, x);
                    (x.ratio(), h.stats().live.map(|l| l.snapshot.peer_stats.live).unwrap_or(0))
                })
                .unwrap_or((0.0, 0));
            // A seeding limit of "No upload" (its own, or the one in Settings) means: don't share it once finished.
            // Kept loaded, it would only sit there as "seeding" while sending nothing, its ratio never growing.
            let no_seed = it.seeding_limit_kbps < 0 || s.torrent_seeding_limit_kbps < 0;
            if target <= 0.0 || ratio >= target || no_seed {
                let _ = session.pause(&h).await;
                mgr.with_item(&it.id, |x| {
                    x.note = if no_seed {
                        "not shared (seeding limit is No upload)".into()
                    } else if ratio >= 0.01 {
                        format!("shared {ratio:.2}×")
                    } else {
                        String::new()
                    }
                });
            } else {
                mgr.with_item(&it.id, |x| {
                    x.seeding = true;
                    x.upload_speed = now.live.as_ref().map(|l| l.upload_speed.mbps * 1048576.0).unwrap_or(0.0);
                    x.connected_peers = peers as i32;
                    x.note = format!("seeding · ratio {ratio:.2} of {target} · {peers} peers");
                });
            }
        }
        self.seed_baseline.lock().retain(|h, _| seeding.contains(h));
    }

    pub async fn shutdown(&self) {
        let Some(s) = self.session_now() else { return };
        let handles: Vec<ManagedTorrentHandle> = s.with_torrents(|it| it.map(|(_, h)| h.clone()).collect());
        for h in handles {
            if !h.is_paused() {
                let _ = s.pause(&h).await;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), s.stop()).await;
    }
}

fn remove_empty_dirs(p: &Path) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                remove_empty_dirs(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(p);
}

/// Same drive (or share) — a move there is a rename, not a copy.
pub fn same_drive(a: &str, b: &str) -> bool {
    let root = |p: &str| p.trim().chars().take(2).collect::<String>().to_lowercase();
    root(a) == root(b)
}

fn move_targets(it: &Item, folder: &str) -> Vec<(PathBuf, PathBuf)> {
    let meta = std::fs::read(&it.torrent_file).ok().and_then(|b| parse(&b).ok());
    match meta {
        Some(m) if m.multi => vec![(PathBuf::from(output_folder(it, &m)), Path::new(folder).join(net::sanitize(&m.name)))],
        Some(m) => m.files.iter().map(|(p, _)| (Path::new(&it.save_folder).join(p), Path::new(folder).join(p))).collect(),
        None => vec![(PathBuf::from(it.full_path()), Path::new(folder).join(&it.file_name))],
    }
}

pub fn check_move_destination(it: &Item, folder: &str) -> DlResult<()> {
    check_move_targets(&move_targets(it, folder)).map_err(DlError::from)
}

fn check_move_targets(paths: &[(PathBuf, PathBuf)]) -> std::io::Result<()> {
    for (_, to) in paths {
        if to.try_exists()? {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("The destination already contains “{}”. Choose another folder or move that file first.", to.file_name().unwrap_or_default().to_string_lossy())));
        }
    }
    Ok(())
}

fn copy_path(from: &Path, to: &Path, created: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir(to)?;
        created.push(to.to_path_buf());
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_path(&entry.path(), &to.join(entry.file_name()), created)?;
        }
    } else {
        let mut source = std::fs::File::open(from)?;
        let mut dest = std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
        created.push(to.to_path_buf());
        std::io::copy(&mut source, &mut dest)?;
        dest.sync_all()?;
    }
    Ok(())
}

fn move_paths(paths: &[(PathBuf, PathBuf)]) -> std::io::Result<()> {
    check_move_targets(paths)?;
    let existing: Vec<_> = paths.iter().filter(|(from, _)| from.exists()).cloned().collect();
    let paths = existing.as_slice();
    // A single rename is atomic. Multiple paths use the same copy transaction as a cross-drive move.
    if let [(from, to)] = paths {
        if !from.exists() { return Ok(()); }
        if let Some(parent) = to.parent() { std::fs::create_dir_all(parent)?; }
        if rename_new(from, to).is_ok() { return Ok(()); }
    }
    copy_paths(paths)?;
    for (from, _) in paths.iter().filter(|(from, _)| from.exists()) {
        let result = if from.is_dir() { std::fs::remove_dir_all(from) } else { std::fs::remove_file(from) };
        // The complete copy is now the usable location, even if Windows keeps an original file open.
        if let Err(e) = result { crate::util::log_error(&format!("Files moved, but an original could not be removed: {e}")); }
    }
    Ok(())
}

fn copy_paths(paths: &[(PathBuf, PathBuf)]) -> std::io::Result<()> {
    let mut created = vec![];
    let result = (|| {
        for (from, to) in paths {
            if let Some(parent) = to.parent() { std::fs::create_dir_all(parent)?; }
            copy_path(from, to, &mut created)?;
        }
        Ok(())
    })();
    if result.is_err() {
        for path in created.iter().rev() {
            let _ = if path.is_dir() { std::fs::remove_dir(path) } else { std::fs::remove_file(path) };
        }
    }
    result
}

#[cfg(windows)]
fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), MOVE_FILE_FLAGS(0)) }.map_err(std::io::Error::other)
}

#[cfg(not(windows))]
fn rename_new(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("Copy required"))
}

// ------------------------------------------------------------------ runner

pub async fn run(mgr: Arc<Manager>, run: Arc<Run>) {
    let id = run.id.clone();
    let saved = mgr.with_item(&id, |it| it.downloaded).unwrap_or(0);
    run.current.store(saved, Ordering::Relaxed); // show what we had, not 0%, while it starts up
    let mut handle: Option<ManagedTorrentHandle> = None;
    let r = go(&mgr, &run, &mut handle).await;
    let done = run.current.load(Ordering::Relaxed);
    let pause = |h: &ManagedTorrentHandle| {
        let h = h.clone();
        let s = mgr.torrents.session_now();
        async move {
            if let Some(s) = s {
                if !h.is_paused() {
                    let _ = s.pause(&h).await;
                }
            }
        }
    };
    match r {
        Ok(()) => {}
        Err(e) if e.kind == ErrKind::Cancelled || run.is_cancelled() => {
            if let Some(h) = &handle {
                pause(h).await;
            }
            mgr.with_item(&id, |it| {
                it.downloaded = done;
                it.note.clear();
                it.set_status(DlStatus::Paused);
            });
        }
        Err(e) => {
            if let Some(h) = &handle {
                pause(h).await;
            }
            mgr.with_item(&id, |it| {
                it.downloaded = done;
                it.note.clear();
                it.error_message = e.msg.clone();
                it.set_status(DlStatus::Error);
            });
        }
    }
}

/// The manager re-splits this share whenever the running set changes.
fn own_down_limit(mgr: &Arc<Manager>, it: &Item) -> i64 {
    mgr.run_of(&it.id).map(|r| r.limiter.get()).unwrap_or(0)
}

fn class_upload_limit(own: i32, global: i32, count: usize) -> Option<NonZeroU32> {
    if own < 0 || global < 0 { return NO_UPLOAD; }
    let share = if global > 0 { (global as i64 * 1024 / count.max(1) as i64).max(2) } else { 0 };
    rate([own as i64 * 1024, share].into_iter().filter(|v| *v > 0).min().unwrap_or(0))
}

async fn go(mgr: &Arc<Manager>, run: &Arc<Run>, out: &mut Option<ManagedTorrentHandle>) -> DlResult<()> {
    let id = run.id.clone();
    let svc = &mgr.torrents;
    mgr.with_item(&id, |it| {
        it.set_status(DlStatus::Connecting);
        it.error_message.clear();
        it.note.clear();
        it.last_try = Some(chrono::Local::now());
    });
    let it = mgr.item(&id).ok_or_else(DlError::cancelled)?;
    let source = if Path::new(&it.torrent_file).is_file() { it.torrent_file.clone() } else { it.url.clone() };
    let status = |s: &str| {
        mgr.with_item(&id, |it| it.error_message = s.to_string());
    };
    let (bytes, cached) = tokio::select! {
        r = svc.resolve(mgr, &source, &it.headers, status) => r?,
        _ = run.cancel.cancelled() => return Err(DlError::cancelled()),
    };
    let meta = parse(&bytes)?;
    if !mgr.claim_torrent_hash(&id, &meta.hash) {
        return Err(DlError::cancelled());
    }
    let skipped: HashSet<i32> = it.skipped_files.iter().copied().collect();
    let wanted: i64 = meta.files.iter().enumerate().filter(|(i, _)| !skipped.contains(&(*i as i32))).map(|(_, f)| f.1 as i64).sum();
    mgr.with_item(&id, |it| {
        it.torrent_file = cached.clone();
        it.info_hash = meta.hash.clone();
        it.file_name = net::sanitize(&meta.name);
        it.total_size = wanted;
        it.resumable = true;
        it.error_message.clear();
    });
    let it = mgr.item(&id).ok_or_else(DlError::cancelled)?;
    let folder = output_folder(&it, &meta);
    std::fs::create_dir_all(&folder)?;
    if let Some(free) = crate::win::free_space(&it.save_folder) {
        let need = wanted - run.current.load(Ordering::Relaxed);
        if free < need + 64 * 1024 * 1024 {
            return Err(DlError::fatal(format!("Not enough space — needs {}, {} free", fmt::size(need), fmt::size(free))));
        }
    }

    // The current share also applies before the first heartbeat.
    let own = own_down_limit(mgr, &it);
    run.started_limit.store(own, Ordering::Relaxed);
    let session = svc.session(mgr).await?;
    let h = svc.add(mgr, &session, bytes, &meta, &folder, &it, own, false).await?;
    if h.is_paused() {
        session.unpause(&h).await.map_err(anyhow_err)?;
    }
    *out = Some(h.clone());
    mgr.with_item(&id, |it| it.set_status(DlStatus::Downloading));

    let (mut last_fetched, mut last_up) = (None::<u64>, None::<u64>);
    let mut ticks = 0u32;
    loop {
        ticks = ticks.wrapping_add(1);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            _ = run.cancel.cancelled() => return Err(DlError::cancelled()),
        }
        let st = h.stats();
        if let Some(err) = &st.error {
            if matches!(st.state, librqbit::TorrentStatsState::Error) {
                return Err(DlError::transient(err.lines().next().unwrap_or(err).to_string()));
            }
        }
        let wanted_now: i64 = st.total_bytes as i64;
        if wanted_now > 0 && wanted_now != wanted {
            mgr.with_item(&id, |it| it.total_size = wanted_now);
        }
        let (note, peers, up_speed) = match (&st.state, &st.live) {
            (librqbit::TorrentStatsState::Initializing { .. }, _) => {
                let pct = if st.total_bytes > 0 { st.progress_bytes as f64 * 100.0 / st.total_bytes as f64 } else { 0.0 };
                (format!("checking what's already on disk {pct:.0}%"), 0, 0.0)
            }
            (_, Some(l)) => {
                let ps = &l.snapshot.peer_stats;
                let fetched = l.snapshot.fetched_bytes;
                let d = last_fetched.map(|p| fetched.saturating_sub(p)).unwrap_or(0);
                last_fetched = Some(fetched);
                run.bytes.fetch_add(d as i64, Ordering::Relaxed);
                run.current.store(st.progress_bytes as i64, Ordering::Relaxed);
                let upb = st.uploaded_bytes;
                let du = last_up.map(|p| upb.saturating_sub(p)).unwrap_or(0) as i64;
                last_up = Some(upb);
                let up = l.upload_speed.mbps * 1048576.0;
                mgr.with_item(&id, |it| {
                    if du > 0 {
                        it.total_uploaded += du;
                    }
                    if d > 0 || du > 0 {
                        it.last_activity_at = Some(chrono::Local::now());
                    }
                    it.active_seconds += 0.5;
                });
                let mut n = format!("{} peers · {} known", ps.live, ps.seen);
                if up > 1024.0 {
                    n.push_str(&format!(" · ↑ {}", fmt::speed(up)));
                }
                (n, ps.live, up)
            }
            _ => ("starting".into(), 0, 0.0),
        };
        run.active.store(peers as i32, Ordering::Relaxed);
        let refresh = ticks % 4 == 1;
        mgr.with_item(&id, |it| {
            if refresh {
                swarm(&h, it);
            }
            it.note = note;
            it.upload_speed = up_speed;
            it.connected_peers = peers as i32;
        });
        if st.finished && matches!(st.state, librqbit::TorrentStatsState::Live | librqbit::TorrentStatsState::Paused) {
            break;
        }
    }

    let s = mgr.settings();
    let own_seed_limit = mgr.with_item(&id, |it| it.seeding_limit_kbps).unwrap_or(0);
    // Ratio 0, or a seeding limit of "No upload": it isn't shared once finished.
    let seed = s.torrent_seed_ratio > 0.0 && own_seed_limit >= 0 && s.torrent_seeding_limit_kbps >= 0;
    if !seed {
        let _ = session.pause(&h).await;
    }
    // The engine leaves empty placeholders for files you unticked; don't leave them lying around.
    for i in &skipped {
        if let Some((p, _)) = meta.files.get(*i as usize) {
            let path = Path::new(&folder).join(p);
            if std::fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(false) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let total = h.stats().total_bytes as i64;
    mgr.with_item(&id, |it| {
        if total > 0 {
            it.total_size = total;
        }
        it.downloaded = it.total_size;
        it.error_message.clear();
        it.note = if seed { "seeding".into() } else { String::new() };
        it.set_status(DlStatus::Completed);
        it.completed_at = Some(chrono::Local::now());
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_budgets_are_independent_for_downloading_and_seeding() {
        assert_eq!(class_upload_limit(0, -1, 2), NO_UPLOAD);
        assert_eq!(class_upload_limit(0, 20, 2).unwrap().get(), 10 * 1024);
        assert_eq!(class_upload_limit(4, 20, 2).unwrap().get(), 4 * 1024);
        assert_eq!(class_upload_limit(-1, 20, 2), NO_UPLOAD);
        assert_eq!(class_upload_limit(0, 0, 2), None);
        assert_eq!(class_upload_limit(0, 1, 2).unwrap().get(), 512);
        let mgr = Manager::new();
        let down = Item { is_torrent: true, status: DlStatus::Downloading, ..Default::default() };
        let seed = Item { is_torrent: true, status: DlStatus::Completed, ..Default::default() };
        {
            let mut st = mgr.st.lock();
            st.settings.torrent_upload_limit_kbps = -1;
            st.settings.torrent_seeding_limit_kbps = 20;
            st.items = vec![down.clone(), seed.clone()];
        }
        assert_eq!(mgr.torrents.upload_limit(&mgr, &down), NO_UPLOAD);
        assert_eq!(mgr.torrents.upload_limit(&mgr, &seed).unwrap().get(), 20 * 1024);
    }

    #[test]
    fn details_cache_reuses_metadata_and_invalidates_changed_files() {
        let path = std::env::current_dir().unwrap().join("target").join(format!("meta-test-{}.torrent", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"d4:infod6:lengthi3e4:name4:test12:piece lengthi16384e6:pieces20:abcdefghijklmnopqrstee").unwrap();
        let svc = Service::default();
        let key = path.to_str().unwrap();
        let first = svc.cached_meta(key).unwrap();
        let again = svc.cached_meta(key).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        std::fs::write(&path, b"d4:infod6:lengthi3e4:name9:test-file12:piece lengthi16384e6:pieces20:abcdefghijklmnopqrstee").unwrap();
        let changed = svc.cached_meta(key).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(changed.name, "test-file");
        std::fs::remove_file(&path).unwrap();
        assert!(svc.cached_meta(key).is_none());
    }

    #[test]
    fn magnet_hash_is_normalized_without_metadata() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(source_hash(&format!("magnet:?xt=urn:btih:{}", hash.to_uppercase())).as_deref(), Some(hash));
    }

    #[test]
    fn move_conflicts_and_copy_failure_keep_originals() {
        let root = std::env::current_dir().unwrap().join("target").join(format!("move-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("a");
        let b = root.join("b");
        let x = root.join("x");
        let y = root.join("y");
        std::fs::write(&a, "first").unwrap();
        std::fs::write(&b, "second").unwrap();
        std::fs::write(&y, "existing").unwrap();
        let paths = vec![(a.clone(), x.clone()), (b.clone(), y.clone())];
        let e = move_paths(&paths).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(e.to_string().contains('y'));
        assert!(!x.exists());
        // Simulate a destination appearing between preflight and the second copy.
        assert!(copy_paths(&paths).is_err());
        assert!(!x.exists());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "second");
        assert_eq!(std::fs::read_to_string(&y).unwrap(), "existing");
        std::fs::remove_file(&y).unwrap();
        move_paths(&paths).unwrap();
        assert!(!a.exists() && !b.exists());
        assert_eq!(std::fs::read_to_string(&x).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&y).unwrap(), "second");
        std::fs::remove_dir_all(root).unwrap();
    }
}
