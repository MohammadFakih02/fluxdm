//! Owns the download list, the running downloads, queues, the scheduler and persistence.
//! Downloads run as tokio tasks and update their list entry through `with_item`.

use crate::model::*;
use crate::util::Limiter;
use crate::{net, store, win};
use chrono::{Datelike, Local, Timelike};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub struct State {
    pub items: Vec<Item>,
    pub queues: Vec<QueueDef>,
    pub settings: Settings,
    pub stats: StatsStore,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Http,
    Video,
    Torrent,
}

#[derive(Default)]
struct SpeedBook {
    last_bytes: i64,
    last_t: Option<Instant>,
    smooth: f64,
}

/// One running download (any kind). The runner task updates the counters; the manager's tick reads them.
pub struct Run {
    pub id: String,
    pub kind: Kind,
    pub cancel: CancellationToken,
    /// Bytes moved this run (for the speed readout).
    pub bytes: AtomicI64,
    /// How much of the file is done right now.
    pub current: AtomicI64,
    pub active: AtomicI32,
    /// Connections the server refused this run (429/503).
    pub throttled: AtomicI32,
    /// yt-dlp's best estimate of the total size.
    pub total_hint: AtomicI64,
    /// This item's own speed limit.
    pub limiter: Limiter,
    /// The limit a yt-dlp/torrent download started with — they can only take a new one by restarting.
    pub started_limit: AtomicI64,
    restart_requested: AtomicBool,
    ended: watch::Sender<bool>,
    book: Mutex<SpeedBook>,
}

impl Run {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

type UiFn = Box<dyn Fn(&str, Value) + Send + Sync>;

pub struct Manager {
    pub st: Mutex<State>,
    running: Mutex<HashMap<String, Arc<Run>>>,
    pub global_limiter: Limiter,
    queue_limiters: Mutex<HashMap<String, Arc<Limiter>>>,
    pub torrents: crate::torrent::Service,
    ui: Mutex<Option<UiFn>>,
    dirty: AtomicBool,
    tick_no: AtomicU64,
    keep_awake: Mutex<Option<bool>>,
    last_tick: Mutex<Option<Instant>>,
    pub total_speed: Mutex<f64>,
    stalled_since: Mutex<HashMap<String, Instant>>,
    last_stall_restart: Mutex<HashMap<String, Instant>>,
    /// Last row JSON sent to the window, per item — only changed rows are sent again.
    sent: Mutex<HashMap<String, String>>,
    sent_order: Mutex<Vec<String>>,
    sent_summary: Mutex<String>,
    pub exiting: AtomicBool,
}

const STALL_THRESHOLD: Duration = Duration::from_secs(100);
const STALL_COOLDOWN: Duration = Duration::from_secs(180);

impl Manager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            st: Mutex::new(State { items: vec![], queues: vec![], settings: Settings::default(), stats: StatsStore::default() }),
            running: Mutex::new(HashMap::new()),
            global_limiter: Limiter::default(),
            queue_limiters: Mutex::new(HashMap::new()),
            torrents: crate::torrent::Service::default(),
            ui: Mutex::new(None),
            dirty: AtomicBool::new(false),
            tick_no: AtomicU64::new(0),
            keep_awake: Mutex::new(None),
            last_tick: Mutex::new(None),
            total_speed: Mutex::new(0.0),
            stalled_since: Mutex::new(HashMap::new()),
            last_stall_restart: Mutex::new(HashMap::new()),
            sent: Mutex::new(HashMap::new()),
            sent_order: Mutex::new(vec![]),
            sent_summary: Mutex::new(String::new()),
            exiting: AtomicBool::new(false),
        })
    }

    pub fn set_ui(&self, f: UiFn) {
        *self.ui.lock() = Some(f);
    }

    pub fn emit(&self, event: &str, payload: Value) {
        if let Some(f) = self.ui.lock().as_ref() {
            f(event, payload);
        }
    }

    pub fn toast(&self, title: &str, body: &str, reveal: Option<String>) {
        self.emit("toast", json!({ "title": title, "body": body, "reveal": reveal }));
    }

    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn settings(&self) -> Settings {
        self.st.lock().settings.clone()
    }

    pub fn with_item<R>(&self, id: &str, f: impl FnOnce(&mut Item) -> R) -> Option<R> {
        let mut st = self.st.lock();
        st.items.iter_mut().find(|i| i.id == id).map(f)
    }

    pub fn item(&self, id: &str) -> Option<Item> {
        self.with_item(id, |i| i.clone())
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.running.lock().contains_key(id)
    }

    pub fn run_of(&self, id: &str) -> Option<Arc<Run>> {
        self.running.lock().get(id).cloned()
    }

    pub fn running_count(&self) -> usize {
        self.running.lock().len()
    }

    // ------------------------------------------------------------------ limits

    pub fn queue_limiter(&self, queue: &str) -> Option<Arc<Limiter>> {
        if queue.is_empty() {
            return None;
        }
        Some(self.queue_limiters.lock().entry(queue.to_string()).or_default().clone())
    }

    /// True while the speed schedule's time window is in effect.
    pub fn schedule_active(s: &Settings) -> bool {
        let now = Local::now();
        s.speed_schedule_enabled && in_window(&s.schedule_from, &s.schedule_to, (now.hour() * 60 + now.minute()) as i32)
    }

    /// Global limit after applying the speed schedule (the stricter of the two wins), KB/s.
    pub fn effective_global_kbps(s: &Settings) -> i32 {
        let g = s.global_speed_limit_kbps;
        if !Self::schedule_active(s) {
            return g;
        }
        let sch = s.schedule_limit_kbps;
        if g <= 0 { sch } else if sch <= 0 { g } else { g.min(sch) }
    }

    /// Waits as needed to respect the global, queue and per-item limits.
    pub async fn throttle(&self, run: &Run, queue: &str, n: usize) {
        let mut wait = self.global_limiter.take(n);
        if let Some(q) = self.queue_limiter(queue) {
            wait = wait.max(q.take(n));
        }
        wait = wait.max(run.limiter.take(n));
        if !wait.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = run.cancel.cancelled() => {}
            }
        }
    }

    /// The tightest limit currently applying to this item, in bytes/s (0 = none).
    pub fn effective_limit(&self, run: &Run, queue: &str) -> i64 {
        let mut best = 0i64;
        let mut take = |v: i64| {
            if v > 0 && (best == 0 || v < best) {
                best = v;
            }
        };
        take(self.global_limiter.get());
        take(self.queue_limiter(queue).map(|q| q.get()).unwrap_or(0));
        take(run.limiter.get());
        best
    }

    // ------------------------------------------------------------------ persistence

    pub fn load(self: &Arc<Self>) {
        let mut settings: Settings = store::read("settings.json").unwrap_or_default();
        if settings.remote_pin.len() < 4 {
            settings.remote_pin = crate::util::random_pin();
        }
        settings.start_with_windows = win::starts_with_windows();
        // The old, short default list missed many file types; anyone still on it gets the new one.
        const OLD_CLIP_EXTS: &str = "zip rar 7z gz xz zst tar iso img exe msi msix apk mp4 mkv avi mov webm m4v mp3 flac wav m4a pdf epub safetensors gguf ckpt pth pt onnx";
        if settings.clipboard_extensions.trim() == OLD_CLIP_EXTS {
            settings.clipboard_extensions = Settings::default().clipboard_extensions;
        }
        // Updates come from the public repository unless another source was set.
        if settings.update_repo.trim().is_empty() {
            settings.update_repo = Settings::default().update_repo;
        }
        let mut queues: Vec<QueueDef> = store::read("queues.json").unwrap_or_default();
        if queues.is_empty() {
            queues.push(QueueDef::default());
        }
        let stats: StatsStore = store::read("stats.json").unwrap_or_default();
        let mut items: Vec<Item> = store::read("downloads.json").unwrap_or_default();
        for it in &mut items {
            *it.segments.lock() = it.saved_segments.iter().cloned().map(|s| Arc::new(Mutex::new(s))).collect();
            if it.status == DlStatus::Verifying && !it.is_torrent && std::path::Path::new(&it.full_path()).exists() {
                it.error_message = "checksum not verified (app was closed)".into();
                it.status = DlStatus::Completed;
            } else if it.is_active() {
                it.status = DlStatus::Paused;
            }
        }
        {
            let mut st = self.st.lock();
            st.settings = settings;
            st.queues = queues;
            st.stats = stats;
            st.items = items;
        }
        let launch: Vec<String> = self.st.lock().queues.iter().filter(|q| q.start_on_launch).map(|q| q.name.clone()).collect();
        for q in launch {
            self.start_queue(&q);
        }
    }

    pub fn save(&self) {
        // Snapshot and write as one step, so a slower older save can't land after a newer one.
        static SAVING: Mutex<()> = Mutex::new(());
        let _g = SAVING.lock();
        let running: HashMap<String, i64> = self
            .running
            .lock()
            .iter()
            .map(|(k, r)| (k.clone(), r.current.load(Ordering::Relaxed)))
            .collect();
        let (items, settings, queues, stats) = {
            let mut st = self.st.lock();
            for it in st.items.iter_mut() {
                if let Some(cur) = running.get(&it.id) {
                    if it.status == DlStatus::Downloading && it.kind() == Kind::Http {
                        it.downloaded = *cur;
                    }
                }
                it.snapshot_segments();
            }
            (st.items.clone(), st.settings.clone(), st.queues.clone(), st.stats.clone())
        };
        let r = store::write("downloads.json", &items)
            .and_then(|_| store::write("settings.json", &settings))
            .and_then(|_| store::write("queues.json", &queues))
            .and_then(|_| store::write("stats.json", &stats));
        match r {
            Ok(()) => self.dirty.store(false, Ordering::Relaxed),
            Err(e) => crate::util::log_error(&format!("Save failed: {e}")),
        }
    }

    // ------------------------------------------------------------------ creating items

    pub fn folder_for(s: &Settings, file_name: &str) -> String {
        if s.category_subfolders {
            std::path::Path::new(&s.default_folder).join(category_of(file_name)).to_string_lossy().into_owned()
        } else {
            s.default_folder.clone()
        }
    }

    /// Picks "name (n).ext" if the name is taken on disk or by another unfinished download.
    pub fn unique_name(&self, folder: &str, name: &str, self_id: Option<&str>, pending: &[Item]) -> String {
        let st = self.st.lock();
        Self::unique_name_in(&st.items, folder, name, self_id, pending)
    }

    fn unique_name_in(items: &[Item], folder: &str, name: &str, self_id: Option<&str>, pending: &[Item]) -> String {
        let mut name = net::sanitize(name);
        if name.is_empty() {
            name = "download".into();
        }
        let p = std::path::Path::new(&name);
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
        let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let taken: HashSet<String> = items
            .iter()
            .chain(pending.iter())
            .filter(|o| Some(o.id.as_str()) != self_id && o.status != DlStatus::Completed)
            .map(|o| o.full_path().to_lowercase())
            .collect();
        for n in 0.. {
            let candidate = if n == 0 { name.clone() } else { format!("{stem} ({n}){ext}") };
            let full = std::path::Path::new(folder).join(&candidate);
            if !full.exists() && !taken.contains(&full.to_string_lossy().to_lowercase()) {
                return candidate;
            }
        }
        unreachable!()
    }

    /// A download with default settings, e.g. one sent straight from the browser extension.
    pub fn create_item(&self, url: &str, file_name: Option<&str>, headers: &str, pending: &[Item]) -> Item {
        let s = self.settings();
        let known = file_name.is_some_and(|n| !n.trim().is_empty());
        let name = if known { file_name.unwrap().to_string() } else { net::name_from_url(url, true).unwrap_or_else(|| "download".into()) };
        let dir = Self::folder_for(&s, &name);
        let mut it = Item {
            url: url.to_string(),
            save_folder: dir.clone(),
            headers: headers.to_string(),
            auto_name: !known,
            ..Default::default()
        };
        it.set_connections(s.default_connections);
        it.file_name = self.unique_name(&dir, &name, None, pending);
        it
    }

    /// A torrent download (magnet link, .torrent link or .torrent file).
    pub fn create_torrent_item(&self, source: &str, name: Option<&str>, folder: Option<&str>, skipped: Option<Vec<i32>>) -> Item {
        let s = self.settings();
        let source = source.trim().trim_matches('"').to_string();
        let display = name
            .map(str::to_string)
            .or_else(|| if net::is_magnet(&source) { net::magnet_name(&source) } else { None })
            .or_else(|| {
                let base = if net::is_http_url(&source) { net::name_from_url(&source, false).unwrap_or_default() } else { source.clone() };
                std::path::Path::new(&base).file_stem().map(|s| s.to_string_lossy().into_owned())
            })
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| "Torrent".into());
        let dir = folder.map(str::to_string).unwrap_or_else(|| {
            if s.category_subfolders {
                std::path::Path::new(&s.default_folder).join("Torrents").to_string_lossy().into_owned()
            } else {
                s.default_folder.clone()
            }
        });
        Item {
            url: source,
            is_torrent: true,
            save_folder: dir,
            file_name: net::sanitize(&display),
            skipped_files: skipped.unwrap_or_default(),
            resumable: true,
            ..Default::default()
        }
    }

    /// A yt-dlp video/audio download. The real file name arrives when it finishes.
    pub fn create_video_item(&self, url: &str, title: Option<&str>, format: &str, playlist: bool, folder: Option<&str>) -> Item {
        let s = self.settings();
        let audio = format.starts_with("audio");
        let dir = folder.map(str::to_string).unwrap_or_else(|| {
            if s.category_subfolders {
                std::path::Path::new(&s.default_folder).join(if audio { "Music" } else { "Video" }).to_string_lossy().into_owned()
            } else {
                s.default_folder.clone()
            }
        });
        let name = match title.filter(|t| !t.trim().is_empty()) {
            Some(t) => net::sanitize(t),
            None => format!("{} from {}", if audio { "Audio" } else { "Video" }, net::host_of(url)),
        };
        let ext = if audio { if format == "audio-mp3" { ".mp3" } else { ".m4a" } } else { ".mp4" };
        Item {
            url: url.to_string(),
            is_video: true,
            video_format: format.to_string(),
            video_playlist: playlist,
            save_folder: dir,
            file_name: name + ext,
            connections: 4,
            resumable: true,
            ..Default::default()
        }
    }

    pub fn add(self: &Arc<Self>, items: Vec<Item>, start: bool) {
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        {
            let mut st = self.st.lock();
            for mut it in items {
                it.status = if !start && !it.queue_name.is_empty() { DlStatus::Queued } else { DlStatus::Paused };
                st.items.insert(0, it);
            }
        }
        if start {
            for id in &ids {
                self.start(id);
            }
        }
        self.pump_queues();
        self.save();
        self.push_list();
        crate::covers::refresh_ids(self, &ids);
    }

    // ------------------------------------------------------------------ start / stop

    pub fn start(self: &Arc<Self>, id: &str) {
        let (kind, limit, queue) = {
            let st = self.st.lock();
            let Some(it) = st.items.iter().find(|i| i.id == id) else { return };
            if it.status == DlStatus::Completed {
                return;
            }
            (it.kind(), it.speed_limit_kbps, it.queue_name.clone())
        };
        let mut running = self.running.lock();
        if running.contains_key(id) {
            return;
        }
        let (tx, _) = watch::channel(false);
        let run = Arc::new(Run {
            id: id.to_string(),
            kind,
            cancel: CancellationToken::new(),
            bytes: AtomicI64::new(0),
            current: AtomicI64::new(0),
            active: AtomicI32::new(0),
            throttled: AtomicI32::new(0),
            total_hint: AtomicI64::new(0),
            limiter: Limiter::default(),
            started_limit: AtomicI64::new(-1),
            restart_requested: AtomicBool::new(false),
            ended: tx,
            book: Mutex::new(SpeedBook { last_t: Some(Instant::now()), ..Default::default() }),
        });
        run.limiter.set(limit as i64 * 1024);
        let _ = queue;
        running.insert(id.to_string(), run.clone());
        drop(running);
        self.with_item(id, |it| {
            it.not_before = None;
            it.set_status(DlStatus::Connecting);
        });
        let mgr = self.clone();
        tauri::async_runtime::spawn(async move {
            match run.kind {
                Kind::Http => crate::http_dl::run(mgr.clone(), run.clone()).await,
                Kind::Video => crate::video::run(mgr.clone(), run.clone()).await,
                Kind::Torrent => crate::torrent::run(mgr.clone(), run.clone()).await,
            }
            mgr.on_ended(&run);
            let _ = run.ended.send(true);
        });
    }

    fn on_ended(self: &Arc<Self>, run: &Arc<Run>) {
        {
            let mut running = self.running.lock();
            if running.get(&run.id).is_some_and(|r| Arc::ptr_eq(r, run)) {
                running.remove(&run.id);
            }
        }
        let settings = self.settings();
        let mut completed: Option<Item> = None;
        let mut failed: Option<Item> = None;
        {
            let mut st = self.st.lock();
            let queue_running: HashSet<String> = st.queues.iter().filter(|q| q.running).map(|q| q.name.clone()).collect();
            let mut file_done = false;
            if let Some(it) = st.items.iter_mut().find(|i| i.id == run.id) {
                it.speed = 0.0;
                it.active_connections = 0;
                it.upload_speed = 0.0;
                if it.status == DlStatus::Completed {
                    it.retry_count = 0;
                    file_done = true;
                    // Browser cookies were only needed to fetch the file; don't keep session tokens on disk.
                    it.headers = it.headers.split('\n').filter(|l| !l.trim_start().to_ascii_lowercase().starts_with("cookie:")).collect::<Vec<_>>().join("\n");
                    completed = Some(it.clone());
                } else if it.status == DlStatus::Error {
                    if queue_running.contains(&it.queue_name) && it.retry_count < settings.queue_retries {
                        it.retry_count += 1;
                        it.not_before = Some(Local::now() + chrono::Duration::seconds(20 * it.retry_count as i64));
                        it.status = DlStatus::Queued;
                    } else {
                        failed = Some(it.clone());
                    }
                } else if !it.status.is_active() {
                } else {
                    // A runner that ended without saying how: treat as paused.
                    it.set_status(DlStatus::Paused);
                }
            }
            if file_done {
                st.stats.file_completed();
            }
        }
        if let Some(it) = completed {
            if settings.notify_on_complete {
                let ok = if !it.expected_hash.is_empty() && it.error_message.is_empty() { "  ✓ checksum OK" } else { "" };
                self.toast("Download complete", &format!("{}{ok}", it.file_name), Some(it.full_path()));
            }
            if it.open_when_done {
                win::open(&it.full_path());
            }
            crate::covers::refresh_ids(self, std::slice::from_ref(&it.id));
        }
        if let Some(it) = failed {
            if settings.notify_on_complete && !self.exiting.load(Ordering::Relaxed) {
                // Kept short and on two lines: a long name or error text overflowed the small notification card.
                let clip = |s: &str, n: usize| -> String {
                    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
                    if one.chars().count() > n { format!("{}…", one.chars().take(n).collect::<String>().trim_end()) } else { one }
                };
                let why = if it.error_message.trim().is_empty() { "Unknown error".to_string() } else { clip(&it.error_message, 85) };
                self.toast("Download failed", &format!("{}\n{}", clip(&it.file_name, 40), why), None);
            }
        }
        self.save();
        self.pump_queues();
        self.push_list();
    }

    /// Stops a download and waits until it has fully let go (so an immediate start isn't ignored).
    pub async fn pause(self: &Arc<Self>, id: &str) {
        if let Some(run) = self.run_of(id) {
            run.cancel.cancel();
            let mut rx = run.ended.subscribe();
            let _ = tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|v| *v)).await;
            return;
        }
        let (torrent_done, queued) = self
            .with_item(id, |it| (it.is_torrent && it.status == DlStatus::Completed, it.status == DlStatus::Queued))
            .unwrap_or((false, false));
        if torrent_done {
            // "Pause" on a finished torrent = stop sharing it.
            self.torrents.stop_seeding(self, id).await;
        } else if queued {
            self.with_item(id, |it| it.status = DlStatus::Paused);
        }
    }

    /// A clean pause+resume (to apply a new limit to yt-dlp/torrents, or to recover a stall).
    pub async fn reconnect(self: &Arc<Self>, id: &str) {
        self.pause(id).await;
        if self.with_item(id, |it| it.status == DlStatus::Paused).unwrap_or(false) {
            self.start(id);
        }
    }

    pub async fn pause_all(self: &Arc<Self>) {
        let ids: Vec<String> = {
            let mut st = self.st.lock();
            for q in st.queues.iter_mut() {
                q.running = false;
            }
            st.items.iter().map(|i| i.id.clone()).collect()
        };
        let futs = ids.iter().map(|id| self.pause(id));
        futures_util::future::join_all(futs).await;
        self.push_list();
    }

    pub async fn remove(self: &Arc<Self>, id: &str, delete_file: bool) {
        self.pause(id).await;
        let Some(it) = self.item(id) else { return };
        self.st.lock().items.retain(|i| i.id != id);
        if it.is_torrent {
            // Unfinished torrents lose their partial data; finished files stay unless asked to delete them.
            self.torrents.remove(self, &it, it.status != DlStatus::Completed).await;
            let still_used = self.st.lock().items.iter().any(|o| o.torrent_file == it.torrent_file);
            if !still_used && !it.torrent_file.is_empty() {
                let _ = std::fs::remove_file(&it.torrent_file);
            }
        } else {
            let _ = std::fs::remove_file(it.part_path());
        }
        if delete_file && it.status == DlStatus::Completed {
            win::recycle(&it.full_path());
        }
        self.save();
        self.push_list();
    }

    pub async fn restart(self: &Arc<Self>, id: &str) {
        self.pause(id).await;
        let Some(it) = self.item(id) else { return };
        if it.is_torrent {
            self.torrents.remove(self, &it, true).await;
        }
        let _ = std::fs::remove_file(it.part_path());
        self.with_item(id, |it| {
            it.segments.lock().clear();
            it.downloaded = 0;
            it.retry_count = 0;
            it.error_message.clear();
            it.status = DlStatus::Paused;
        });
        self.start(id);
    }

    // ------------------------------------------------------------------ queues & scheduler

    pub fn start_queue(self: &Arc<Self>, name: &str) {
        {
            let running: HashSet<String> = self.running.lock().keys().cloned().collect();
            let mut st = self.st.lock();
            for it in st.items.iter_mut() {
                if it.queue_name == name && !running.contains(&it.id) && matches!(it.status, DlStatus::Paused | DlStatus::Error) {
                    it.status = DlStatus::Queued;
                    it.retry_count = 0;
                }
            }
            if let Some(q) = st.queues.iter_mut().find(|q| q.name == name) {
                q.started_any = false;
                q.running = true;
            }
        }
        self.pump_queues();
    }

    pub fn stop_queue(self: &Arc<Self>, name: &str) {
        let to_pause: Vec<String> = {
            let running: HashSet<String> = self.running.lock().keys().cloned().collect();
            let mut st = self.st.lock();
            if let Some(q) = st.queues.iter_mut().find(|q| q.name == name) {
                q.running = false;
            }
            let mut v = vec![];
            for it in st.items.iter_mut().filter(|i| i.queue_name == name) {
                if running.contains(&it.id) {
                    v.push(it.id.clone());
                } else if it.status == DlStatus::Queued {
                    it.status = DlStatus::Paused;
                }
            }
            v
        };
        for id in to_pause {
            let m = self.clone();
            tauri::async_runtime::spawn(async move { m.pause(&id).await });
        }
    }

    pub fn rename_queue(&self, old: &str, new: &str) {
        let mut st = self.st.lock();
        for it in st.items.iter_mut().filter(|i| i.queue_name == old) {
            it.queue_name = new.to_string();
        }
        if let Some(q) = st.queues.iter_mut().find(|q| q.name == old) {
            q.name = new.to_string();
        }
    }

    pub fn delete_queue(self: &Arc<Self>, name: &str) {
        self.stop_queue(name);
        let mut st = self.st.lock();
        for it in st.items.iter_mut().filter(|i| i.queue_name == name) {
            it.queue_name.clear();
        }
        st.queues.retain(|q| q.name != name);
    }

    pub fn queue_members(st: &State, name: &str) -> Vec<String> {
        let mut v: Vec<&Item> = st.items.iter().filter(|i| i.queue_name == name).collect();
        v.sort_by_key(|i| i.queue_order);
        v.into_iter().map(|i| i.id.clone()).collect()
    }

    fn pump_queues(self: &Arc<Self>) {
        let now = Local::now();
        let running: HashSet<String> = self.running.lock().keys().cloned().collect();
        let mut to_start = vec![];
        let mut finished = vec![];
        {
            let mut st = self.st.lock();
            let names: Vec<String> = st.queues.iter().filter(|q| q.running).map(|q| q.name.clone()).collect();
            for name in names {
                let members = Self::queue_members(&st, &name);
                let max = st.queues.iter().find(|q| q.name == name).map(|q| q.max_concurrent).unwrap_or(2);
                let mut active = members.iter().filter(|id| running.contains(*id)).count() as i32;
                let mut started = false;
                for id in &members {
                    if active >= max {
                        break;
                    }
                    let Some(it) = st.items.iter().find(|i| &i.id == id) else { continue };
                    if it.status == DlStatus::Queued && !running.contains(id) && it.not_before.is_none_or(|t| t <= now) {
                        to_start.push(id.clone());
                        started = true;
                        active += 1;
                    }
                }
                let any_queued = members.iter().any(|id| st.items.iter().any(|i| &i.id == id && i.status == DlStatus::Queued));
                if let Some(q) = st.queues.iter_mut().find(|q| q.name == name) {
                    if started {
                        q.started_any = true;
                    }
                    if active == 0 && !any_queued {
                        q.running = false;
                        if q.started_any {
                            finished.push(q.clone());
                        }
                    }
                }
            }
        }
        for id in to_start {
            self.start(&id);
        }
        for q in finished {
            self.on_queue_finished(q);
        }
    }

    fn on_queue_finished(self: &Arc<Self>, q: QueueDef) {
        let s = self.settings();
        if s.notify_on_complete {
            self.toast("Queue finished", &format!("Everything in “{}” is done.", q.name), None);
        }
        if q.post_action != PostAction::None {
            self.save();
            self.emit("countdown", json!({ "action": format!("{:?}", q.post_action), "force": q.force_shutdown, "queue": q.name }));
        }
    }

    fn run_scheduler(self: &Arc<Self>) {
        let now = Local::now();
        let min = (now.hour() * 60 + now.minute()) as i32;
        // The 5-minute window after a set time may spill past midnight (23:58 → 00:02); that spill still
        // belongs to the day the time was set for, so the day is yesterday there.
        let due = |hhmm: &str| {
            parse_hhmm(hhmm).and_then(|t| {
                let since = (min - t).rem_euclid(1440);
                (since < 5).then(|| if min >= t { now.date_naive() } else { now.date_naive() - chrono::Duration::days(1) })
            })
        };
        let mut start = vec![];
        let mut stop = vec![];
        {
            let mut st = self.st.lock();
            for q in st.queues.iter_mut() {
                if q.start_at_enabled && !q.running {
                    if let Some(day) = due(&q.start_at) {
                        let dow = day.weekday().num_days_from_sunday();
                        let today = if q.daily { q.days_mask & (1 << dow) != 0 } else { q.once_date.date_naive() == day };
                        if today && q.last_start_fire.date_naive() != day {
                            q.last_start_fire = now - chrono::Duration::days((now.date_naive() - day).num_days());
                            start.push(q.name.clone());
                        }
                    }
                }
                if q.stop_at_enabled && q.running {
                    if let Some(day) = due(&q.stop_at) {
                        if q.last_stop_fire.date_naive() != day {
                            q.last_stop_fire = now - chrono::Duration::days((now.date_naive() - day).num_days());
                            stop.push(q.name.clone());
                        }
                    }
                }
            }
        }
        for n in &start {
            self.start_queue(n);
        }
        for n in &stop {
            self.stop_queue(n);
        }
        if !start.is_empty() || !stop.is_empty() {
            self.mark_dirty();
        }
    }

    /// Force-reconnects anything that has looked like "Downloading" with zero live connections for too long
    /// (e.g. a brief internet outage). At most one forced reconnect per item every few minutes, so a truly
    /// dead server still reaches Error through its own retry limit instead of being restarted forever.
    fn check_stalls(self: &Arc<Self>) {
        let now = Instant::now();
        let running: HashSet<String> = self.running.lock().keys().cloned().collect();
        let mut stalled = self.stalled_since.lock();
        stalled.retain(|id, _| running.contains(id));
        let mut last = self.last_stall_restart.lock();
        let due: Vec<String> = stalled
            .iter()
            .filter(|(id, since)| now - **since >= STALL_THRESHOLD && last.get(*id).is_none_or(|t| now - *t >= STALL_COOLDOWN))
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            stalled.remove(&id);
            last.insert(id.clone(), now);
            self.with_item(&id, |it| it.error_message = "no connection for a while — reconnecting".into());
            let m = self.clone();
            tauri::async_runtime::spawn(async move { m.reconnect(&id).await });
        }
    }

    // ------------------------------------------------------------------ heartbeat (every 500 ms)

    pub fn tick(self: &Arc<Self>) {
        let tick = self.tick_no.fetch_add(1, Ordering::Relaxed) + 1;
        let now = Instant::now();
        let settings = self.settings();
        self.global_limiter.set(Self::effective_global_kbps(&settings) as i64 * 1024);
        {
            let st = self.st.lock();
            for q in &st.queues {
                if let Some(l) = self.queue_limiter(&q.name) {
                    l.set(q.speed_limit_kbps as i64 * 1024);
                }
            }
        }
        let elapsed = {
            let mut lt = self.last_tick.lock();
            let e = lt.map(|t| (now - t).as_secs_f64()).unwrap_or(0.0);
            *lt = Some(now);
            e
        };

        let runs: Vec<Arc<Run>> = self.running.lock().values().cloned().collect();
        let mut total = 0.0;
        let mut new_bytes = 0i64;
        let mut restarts = vec![];
        {
            let mut st = self.st.lock();
            let mut stalled = self.stalled_since.lock();
            for run in &runs {
                let Some(it) = st.items.iter_mut().find(|i| i.id == run.id) else { continue };
                let b = run.bytes.load(Ordering::Relaxed);
                let mut book = run.book.lock();
                let dt = book.last_t.map(|t| (now - t).as_secs_f64()).unwrap_or(0.0);
                let inst = if dt > 0.0 { (b - book.last_bytes) as f64 / dt } else { 0.0 };
                new_bytes += (b - book.last_bytes).max(0);
                book.last_bytes = b;
                book.last_t = Some(now);
                book.smooth = if book.smooth <= 0.0 { inst } else { book.smooth * 0.85 + inst * 0.15 };
                let smooth = book.smooth;
                drop(book);
                run.limiter.set(it.speed_limit_kbps as i64 * 1024);
                if it.status == DlStatus::Downloading {
                    let hint = run.total_hint.load(Ordering::Relaxed);
                    if run.kind == Kind::Video && hint > 0 {
                        it.total_size = hint;
                    }
                    it.downloaded = run.current.load(Ordering::Relaxed);
                    it.speed = smooth;
                    it.active_connections = run.active.load(Ordering::Relaxed);
                    it.throttled = run.throttled.load(Ordering::Relaxed);
                    // Torrents sit at 0 connections on purpose while checking data on disk; they're left out.
                    if it.active_connections > 0 || it.is_torrent {
                        stalled.remove(&it.id);
                    } else {
                        stalled.entry(it.id.clone()).or_insert(now);
                    }
                } else {
                    stalled.remove(&it.id);
                }
                let downloading = it.status == DlStatus::Downloading;
                it.push_speed(if downloading { smooth } else { 0.0 });
                if downloading {
                    total += smooth;
                }
                // yt-dlp fixes its limit at start: restart it when it changes a lot. Torrents take new
                // limits live (torrent.rs tick); restarting one would re-check its data on disk.
                if run.kind == Kind::Video && !run.restart_requested.load(Ordering::Relaxed) {
                    let started = run.started_limit.load(Ordering::Relaxed);
                    if started >= 0 && run.active.load(Ordering::Relaxed) > 0 {
                        let q = it.queue_name.clone();
                        let global = self.global_limiter.get();
                        let cur = {
                            let mut best = 0i64;
                            for v in [global, self.queue_limiter(&q).map(|l| l.get()).unwrap_or(0), run.limiter.get()] {
                                if v > 0 && (best == 0 || v < best) {
                                    best = v;
                                }
                            }
                            best
                        };
                        let changed = cur != started && (cur == 0 || started == 0 || (cur - started).abs() > started / 10);
                        if changed {
                            run.restart_requested.store(true, Ordering::Relaxed);
                            restarts.push(run.id.clone());
                        }
                    }
                }
            }
            for it in st.items.iter_mut() {
                if !runs.iter().any(|r| r.id == it.id) && !it.speed_history.is_empty() {
                    it.push_speed(0.0);
                }
            }
            // Only seconds in which data actually moved count as "time downloading" (not installs or idle seeding).
            st.stats.record(new_bytes, if new_bytes > 0 { elapsed } else { 0.0 }, total);
        }
        *self.total_speed.lock() = total;
        for id in restarts {
            let m = self.clone();
            tauri::async_runtime::spawn(async move { m.reconnect(&id).await });
        }

        if tick % 2 == 0 {
            self.run_scheduler();
            self.pump_queues();
            self.check_stalls();
            let mgr = self.clone();
            tauri::async_runtime::spawn(async move { mgr.torrents.tick(&mgr).await });
            let want = settings.prevent_sleep && !runs.is_empty();
            let mut ka = self.keep_awake.lock();
            if *ka != Some(want) {
                *ka = Some(want);
                win::keep_awake(want);
            }
        }
        if tick % 20 == 5 {
            crate::covers::sweep(self);
        }
        if tick % 10 == 0 && (!runs.is_empty() || self.dirty.load(Ordering::Relaxed)) {
            self.save();
        }
        self.push_list();
    }

    // ------------------------------------------------------------------ what the window sees

    /// Sends the window every row that changed since last time (plus the order and a summary).
    pub fn push_list(&self) {
        let (rows, order, summary) = self.snapshot();
        let mut sent = self.sent.lock();
        let mut changed = vec![];
        let mut seen = HashSet::new();
        for (id, row) in rows {
            let text = row.to_string();
            seen.insert(id.clone());
            if sent.get(&id) != Some(&text) {
                sent.insert(id, text);
                changed.push(row);
            }
        }
        let removed: Vec<String> = sent.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        for r in &removed {
            sent.remove(r);
        }
        let mut last_order = self.sent_order.lock();
        let order_changed = *last_order != order;
        if order_changed {
            *last_order = order.clone();
        }
        drop(sent);
        drop(last_order);
        // Nothing new (idle, or everything paused): don't wake the windows twice a second.
        let summary_text = summary.to_string();
        {
            let mut last_summary = self.sent_summary.lock();
            if changed.is_empty() && removed.is_empty() && !order_changed && *last_summary == summary_text {
                return;
            }
            *last_summary = summary_text;
        }
        self.emit(
            "list",
            json!({ "changed": changed, "removed": removed, "order": if order_changed { json!(order) } else { Value::Null }, "summary": summary }),
        );
    }

    /// Forget what was sent, so the next push sends everything (a window just (re)loaded).
    pub fn reset_sent(&self) {
        self.sent.lock().clear();
        self.sent_order.lock().clear();
        self.sent_summary.lock().clear();
    }

    fn snapshot(&self) -> (Vec<(String, Value)>, Vec<String>, Value) {
        let running: HashMap<String, Arc<Run>> = self.running.lock().clone();
        let st = self.st.lock();
        let s = &st.settings;
        let mut rows = Vec::with_capacity(st.items.len());
        let mut order = Vec::with_capacity(st.items.len());
        let (mut done, mut total) = (0i64, 0i64);
        for it in &st.items {
            order.push(it.id.clone());
            if it.status == DlStatus::Downloading && it.total_size > 0 {
                done += it.downloaded;
                total += it.total_size;
            }
            rows.push((it.id.clone(), row_json(it, running.contains_key(&it.id), s)));
        }
        let queues: Vec<Value> = st
            .queues
            .iter()
            .map(|q| {
                let members: Vec<&Item> = st.items.iter().filter(|i| i.queue_name == q.name).collect();
                json!({
                    "name": q.name, "running": q.running, "max": q.max_concurrent,
                    "active": members.iter().filter(|i| i.is_active()).count(),
                    "waiting": members.iter().filter(|i| matches!(i.status, DlStatus::Queued | DlStatus::Paused | DlStatus::Error)).count(),
                })
            })
            .collect();
        let speed = *self.total_speed.lock();
        let summary = json!({
            "running": running.len(),
            "count": st.items.len(),
            "speed": speed,
            "speedText": fmt::speed(speed),
            "done": done,
            "total": total,
            "limitKBps": s.global_speed_limit_kbps,
            "effectiveLimitText": fmt::limit(Self::effective_global_kbps(s)),
            "schedule": Self::schedule_active(s),
            "queues": queues,
            "torrent": self.torrents.status_line(),
        });
        (rows, order, summary)
    }
}

impl Item {
    pub fn kind(&self) -> Kind {
        if self.is_torrent {
            Kind::Torrent
        } else if self.is_video {
            Kind::Video
        } else {
            Kind::Http
        }
    }
}

/// One row of the list, as the window shows it.
fn row_json(it: &Item, running: bool, s: &Settings) -> Value {
    let downloading = it.status == DlStatus::Downloading;
    let active = it.is_active();
    let eta = if downloading && it.speed > 1.0 && it.total_size > 0 { fmt::eta((it.total_size - it.downloaded) as f64 / it.speed) } else { String::new() };
    let progress_text = if it.status == DlStatus::Completed || it.total_size > 0 {
        format!("{:.1}%", it.progress())
    } else if it.downloaded > 0 {
        fmt::size(it.downloaded)
    } else {
        String::new()
    };
    let conn = if it.is_torrent {
        if downloading { format!("{} peers", it.active_connections) } else { String::new() }
    } else if downloading {
        format!("{}/{}", it.active_connections, it.connections)
    } else {
        it.connections.to_string()
    };
    let ratio = if !it.is_torrent || it.downloaded <= 0 { String::new() } else if it.ratio() >= 9999.0 { "∞".into() } else { format!("{:.2}", it.ratio()) };
    let icon = crate::covers::icon_url(it, s);
    json!({
        "id": it.id,
        "name": it.file_name,
        "url": it.url,
        "folder": it.save_folder,
        "path": it.full_path(),
        "status": it.status.name(),
        "statusText": it.status_text(),
        "error": it.error_message,
        "progress": (it.progress() * 10.0).round() / 10.0,
        "progressText": progress_text,
        "total": it.total_size,
        "downloaded": it.downloaded,
        "sizeText": if it.total_size > 0 { fmt::size(it.total_size) } else { "?".into() },
        "downloadedText": format!("{} of {}", fmt::size(it.downloaded.max(0)), if it.total_size > 0 { fmt::size(it.total_size) } else { "?".into() }),
        "speed": it.speed.round(),
        "speedText": if downloading { fmt::speed(it.speed) } else { String::new() },
        "upSpeedText": if it.is_torrent && active { fmt::speed(it.upload_speed) } else { String::new() },
        "upSpeed": it.upload_speed.round(),
        "eta": eta,
        "conn": conn,
        "connections": it.connections,
        // Like the .NET build: connected (swarm total from the trackers); peers exclude seeds.
        "seeds": if it.is_torrent && active { format!("{} ({})", it.connected_seeds, it.swarm_seeds.max(it.connected_seeds)) } else { String::new() },
        "peers": if it.is_torrent && active {
            let p = (it.connected_peers - it.connected_seeds).max(0);
            format!("{} ({})", p, it.swarm_peers.max(p))
        } else {
            String::new()
        },
        "availability": if it.is_torrent && active { it.availability.map(|a| format!("{a:.2}")).unwrap_or_default() } else { String::new() },
        "availabilityNum": it.availability.unwrap_or(-1.0),
        "ratio": ratio,
        "category": it.category(),
        "tags": it.tags,
        "queue": it.queue_name,
        "timeActive": fmt::duration(it.active_seconds),
        "activeSeconds": it.active_seconds.round(),
        "lastActivity": it.last_activity_at.as_ref().map(fmt::ago).unwrap_or_default(),
        "added": it.added_at.format("%b %d %H:%M").to_string(),
        "addedTs": it.added_at.timestamp(),
        "torrent": it.is_torrent,
        "video": it.is_video,
        "resumable": it.resumable,
        "running": running,
        "limitKBps": it.speed_limit_kbps,
        "sequential": it.sequential,
        "openWhenDone": it.open_when_done,
        "uploadLimitKBps": it.upload_limit_kbps,
        "seedingLimitKBps": it.seeding_limit_kbps,
        "hash": it.expected_hash,
        "mirrors": it.mirrors,
        "infoHash": it.info_hash,
        "uploaded": it.total_uploaded,
        "icon": icon,
        "cover": crate::covers::cover_url(it, s),
        "throttled": it.throttled,
    })
}
