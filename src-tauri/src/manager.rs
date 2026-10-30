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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct UpdateResume {
    items: Vec<(String, DlStatus)>,
    queues: Vec<(String, HashSet<String>)>,
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
    /// This item's allocated share, tightened by its own speed limit.
    pub limiter: Limiter,
    /// The limit a yt-dlp download started with (fixed for that process).
    pub started_limit: AtomicI64,
    restart_requested: AtomicBool,
    created_at: Instant,
    ended: watch::Sender<bool>,
    book: Mutex<SpeedBook>,
}

impl Run {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    #[cfg(test)]
    pub fn test_run(id: &str) -> Arc<Self> {
        let (ended, _) = watch::channel(false);
        Arc::new(Self {
            id: id.into(), kind: Kind::Http, cancel: CancellationToken::new(),
            bytes: AtomicI64::new(0), current: AtomicI64::new(0), active: AtomicI32::new(0), throttled: AtomicI32::new(0),
            total_hint: AtomicI64::new(0), limiter: Limiter::default(), started_limit: AtomicI64::new(-1),
            restart_requested: AtomicBool::new(false), created_at: Instant::now(), ended, book: Mutex::new(SpeedBook::default()),
        })
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
    last_scheduler: Mutex<Option<chrono::NaiveDateTime>>,
    pub total_speed: Mutex<f64>,
    stalled_since: Mutex<HashMap<String, Instant>>,
    last_stall_restart: Mutex<HashMap<String, Instant>>,
    /// Last row JSON sent to the window, per item — only changed rows are sent again.
    sent: Mutex<HashMap<String, String>>,
    sent_order: Mutex<Vec<String>>,
    sent_summary: Mutex<String>,
    pub exiting: AtomicBool,
    watched: Arc<tokio::sync::Mutex<crate::watched::Watcher>>,
}

const STALL_THRESHOLD: Duration = Duration::from_secs(100);
const STALL_COOLDOWN: Duration = Duration::from_secs(180);

/// Progressive filling freezes only consumers that hit demand, item or queue caps.
fn download_allocations(demand: &[i64], groups: &[usize], queues: &[i64], global: i64) -> Vec<i64> {
    let cap = |v: i64| if v > 0 { v as f64 } else { f64::INFINITY };
    let mut allocated = vec![0.0; demand.len()];
    let mut active = vec![true; demand.len()];
    loop {
        let count = active.iter().filter(|v| **v).count();
        if count == 0 { break; }
        let mut step = (cap(global) - allocated.iter().sum::<f64>()) / count as f64;
        for (i, on) in active.iter().enumerate() {
            if *on { step = step.min(cap(demand[i]) - allocated[i]); }
        }
        for (q, limit) in queues.iter().enumerate() {
            let count = (0..demand.len()).filter(|i| active[*i] && groups[*i] == q).count();
            if count > 0 {
                let used: f64 = allocated.iter().enumerate().filter(|(i, _)| groups[*i] == q).map(|(_, v)| v).sum();
                step = step.min((cap(*limit) - used) / count as f64);
            }
        }
        for (i, on) in active.iter().enumerate() {
            if *on { allocated[i] += step.max(0.0); }
        }
        let global_full = allocated.iter().sum::<f64>() >= cap(global) - 0.5;
        let queue_full: Vec<bool> = queues.iter().enumerate().map(|(q, limit)| {
            allocated.iter().enumerate().filter(|(i, _)| groups[*i] == q).map(|(_, v)| v).sum::<f64>() >= cap(*limit) - 0.5
        }).collect();
        for i in 0..active.len() {
            if global_full || queue_full[groups[i]] || allocated[i] >= cap(demand[i]) - 0.5 { active[i] = false; }
        }
    }
    allocated.into_iter().map(|v| if v.is_infinite() { 0 } else { (v as i64).max(1) }).collect()
}

fn video_budget_changed(started: i64, current: i64, elapsed: Duration) -> bool {
    elapsed >= Duration::from_secs(60) && started >= 0 && started != current
        && (started == 0 || current == 0 || (current - started).abs() as f64 > started as f64 * 0.30)
}

fn load_queue_defs(raw: Vec<Value>, legacy_retries: Option<i32>) -> Vec<QueueDef> {
    raw.into_iter().filter_map(|value| {
        let missing_retries = value.get("RetryCount").is_none();
        let mut q: QueueDef = serde_json::from_value(value).ok()?;
        if missing_retries {
            q.retry_count = legacy_retries.unwrap_or(0).clamp(0, 100);
        }
        Some(q)
    }).collect()
}

// Slot identity is the scheduled local date, independent of UTC offsets during DST changes.
fn scheduled_start(q: &QueueDef, now: chrono::NaiveDateTime) -> Option<chrono::NaiveDate> {
    if !q.start_at_enabled { return None; }
    let start = parse_hhmm(&q.start_at)?;
    let minute = (now.hour() * 60 + now.minute()) as i32;
    let day = if minute >= start { now.date() } else { now.date().pred_opt()? };
    let stop = if q.stop_at_enabled { parse_hhmm(&q.stop_at)? } else { 1440 };
    let elapsed = if minute >= start { minute - start } else { minute + 1440 - start };
    let duration = if !q.stop_at_enabled { 1440 } else if stop > start { stop - start } else { stop + 1440 - start };
    if elapsed >= duration { return None; }
    let enabled = if q.daily { q.days_mask & (1 << day.weekday().num_days_from_sunday()) != 0 }
        else { q.once_date.date_naive() == day };
    let last = q.last_start_slot.unwrap_or_else(|| q.last_start_fire.date_naive());
    (enabled && last < day).then_some(day)
}

pub fn preserve_queue_runtime(parsed: &mut QueueDef, old: &QueueDef, now: chrono::NaiveDateTime) {
    parsed.running = old.running;
    parsed.started_ids = old.started_ids.clone();
    parsed.finish_notified = old.finish_notified;
    parsed.schedule_slot = old.schedule_slot;
    parsed.last_start_slot = old.last_start_slot;
    parsed.last_stop_slot = old.last_stop_slot;
    parsed.last_start_fire = old.last_start_fire;
    parsed.last_stop_fire = old.last_stop_fire;
    let changed = parsed.start_at != old.start_at || parsed.days_mask != old.days_mask
        || parsed.daily != old.daily || parsed.once_date != old.once_date || parsed.start_at_enabled != old.start_at_enabled;
    if changed && parsed.start_at_enabled && parse_hhmm(&parsed.start_at).is_some_and(|t| t > (now.hour() * 60 + now.minute()) as i32) {
        // Exclude yesterday's catch-up slot while allowing the new time today.
        parsed.last_start_slot = now.date().pred_opt();
        parsed.last_start_fire = min_when();
    }
}

fn scheduled_stop(q: &QueueDef, now: chrono::NaiveDateTime, previous: Option<chrono::NaiveDateTime>) -> Option<chrono::NaiveDate> {
    if !q.stop_at_enabled { return None; }
    let stop = parse_hhmm(&q.stop_at)?;
    let at = |day: chrono::NaiveDate| day.and_hms_opt((stop / 60) as u32, (stop % 60) as u32, 0);
    let today = at(now.date())?;
    let day = if now >= today { now.date() } else { now.date().pred_opt()? };
    let deadline = at(day)?;
    let last = q.last_stop_slot.unwrap_or_else(|| q.last_stop_fire.date_naive());
    if last >= day { return None; }
    let crossed = previous.is_some_and(|p| p < deadline && now >= deadline);
    let expired_slot = q.schedule_slot.is_some_and(|slot| {
        let start = parse_hhmm(&q.start_at).unwrap_or(0);
        let end_day = if stop <= start { slot.succ_opt().unwrap_or(slot) } else { slot };
        at(end_day).is_some_and(|end| now >= end)
    });
    (crossed || expired_slot || (previous.is_none() && now.signed_duration_since(deadline).num_seconds() < 60)).then_some(day)
}

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
            last_scheduler: Mutex::new(None),
            total_speed: Mutex::new(0.0),
            stalled_since: Mutex::new(HashMap::new()),
            last_stall_restart: Mutex::new(HashMap::new()),
            sent: Mutex::new(HashMap::new()),
            sent_order: Mutex::new(vec![]),
            sent_summary: Mutex::new(String::new()),
            exiting: AtomicBool::new(false),
            watched: Arc::new(tokio::sync::Mutex::new(crate::watched::Watcher::default())),
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

    /// A problem the user should notice (a port in use, a file that couldn't be removed): shown with a warning icon.
    pub fn warn_toast(&self, title: &str, body: &str) {
        self.emit("toast", json!({ "kind": "warning", "title": title, "body": body }));
    }

    fn item_toast(&self, it: &Item, kind: &str, body: String) {
        let s = self.settings();
        self.emit("toast", json!({
            "kind": kind, "id": it.id, "title": if kind == "finished" { format!("{} finished", crate::covers::display_title(it)) } else { "Download failed".into() },
            "fileName": it.file_name, "body": body, "reveal": if kind == "finished" { Some(it.full_path()) } else { None },
            "torrent": it.is_torrent, "game": crate::covers::is_game(it), "artColor": it.art_color, "banner": crate::covers::banner_url(it, &s), "icon": crate::covers::icon_url(it, &s),
            "size": fmt::size(it.total_size), "duration": fmt::duration(it.active_seconds),
            "checksum": !it.expected_hash.is_empty() && it.error_message.is_empty(),
        }));
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

    pub fn add_queue_choice(&self, requested: Option<&str>) -> String {
        let st = self.st.lock();
        let name = requested.unwrap_or(&st.settings.last_add_queue);
        if st.queues.iter().any(|q| q.name == name) { name.to_string() } else { String::new() }
    }

    pub fn remember_add_queue(&self, name: &str) {
        let mut st = self.st.lock();
        st.settings.last_add_queue = if st.queues.iter().any(|q| q.name == name) { name.to_string() } else { String::new() };
    }

    pub fn queue_limiter(&self, queue: &str) -> Option<Arc<Limiter>> {
        if queue.is_empty() {
            return None;
        }
        Some(self.queue_limiters.lock().entry(queue.to_string()).or_default().clone())
    }

    /// True while the speed schedule's time window is in effect.
    pub fn schedule_active(s: &Settings) -> bool {
        let now = Local::now();
        Self::schedule_active_at(s, now.naive_local())
    }

    pub fn schedule_active_at(s: &Settings, now: chrono::NaiveDateTime) -> bool {
        let minute = (now.hour() * 60 + now.minute()) as i32;
        let from = parse_hhmm(&s.schedule_from).unwrap_or(0);
        let to = parse_hhmm(&s.schedule_to).unwrap_or(0);
        let day = if from > to && minute < to { now.date().pred_opt().unwrap_or(now.date()) } else { now.date() };
        s.speed_schedule_enabled && s.schedule_days_mask & (1 << day.weekday().num_days_from_sunday()) != 0
            && in_window(&s.schedule_from, &s.schedule_to, minute)
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

    /// Share spare capacity across engines, with growth headroom for idle consumers.
    fn refresh_limits(&self) {
        let runs = self.running.lock().clone();
        let st = self.st.lock();
        let consumers: Vec<&Item> = st.items.iter().filter(|i| runs.contains_key(&i.id)).collect();
        let global = Self::effective_global_kbps(&st.settings) as i64 * 1024;
        self.global_limiter.set(global);
        for q in &st.queues {
            if let Some(l) = self.queue_limiter(&q.name) { l.set(q.speed_limit_kbps as i64 * 1024); }
        }
        let mut queue_names = vec![String::new()];
        let mut budgets = vec![0];
        for q in &st.queues {
            if !q.name.is_empty() { queue_names.push(q.name.clone()); budgets.push(q.speed_limit_kbps as i64 * 1024); }
        }
        let groups: Vec<usize> = consumers.iter().map(|it| queue_names.iter().position(|q| q == &it.queue_name).unwrap_or(0)).collect();
        let demands: Vec<i64> = consumers.iter().enumerate().map(|(i, it)| {
            let run = &runs[&it.id];
            let book = run.book.lock();
            let started = run.started_limit.load(Ordering::Relaxed);
            // A busy fixed-rate process cannot grow into headroom without a reconnect.
            let busy_video = run.kind == Kind::Video && started > 0 && book.smooth >= started as f64 * 0.8;
            let measured = if (global > 0 || budgets[groups[i]] > 0) && book.last_t.is_some() && !busy_video {
                ((book.smooth * 1.25) as i64).max(4096)
            } else { 0 };
            [measured, it.speed_limit_kbps as i64 * 1024].into_iter().filter(|v| *v > 0).min().unwrap_or(0)
        }).collect();
        for (it, limit) in consumers.iter().zip(download_allocations(&demands, &groups, &budgets, global)) {
            runs[&it.id].limiter.set(limit);
        }
    }

    // ------------------------------------------------------------------ persistence

    pub fn load(self: &Arc<Self>, updated: bool) {
        let mut settings: Settings = store::read("settings.json").unwrap_or_default();
        settings.normalize_appearance();
        if let Err(e) = net::configure_proxy(&settings) { crate::util::log_error(&e); }
        if settings.remote_pin.len() < 4 {
            settings.remote_pin = crate::util::random_pin();
        }
        // settings.json keeps what the user chose; the registry entry is only the means. If it was on but the
        // entry is gone or points at another copy (an update or reinstall), put it back for this exe.
        // Not for an isolated copy (FLUXDM_DATA set): a test build must never take over the real startup entry.
        if settings.start_with_windows && std::env::var_os("FLUXDM_DATA").is_none() && !win::starts_with_windows() {
            win::set_start_with_windows(true);
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
        let legacy_retries = store::read::<Value>("settings.json")
            .and_then(|s| s.get("QueueRetries").and_then(Value::as_i64)).map(|n| n.clamp(0, 100) as i32);
        let mut queues = load_queue_defs(store::read("queues.json").unwrap_or_default(), legacy_retries);
        if queues.is_empty() {
            queues.push(QueueDef::default());
        }
        let stats: StatsStore = store::read("stats.json").unwrap_or_default();
        let mut items: Vec<Item> = store::read("downloads.json").unwrap_or_default();
        for it in &mut items {
            *it.segments.lock() = it.saved_segments.iter().cloned().map(|s| Arc::new(Mutex::new(s))).collect();
            if it.is_active() {
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
        self.start_loaded(updated, &store::path("update-resume.json"));
    }

    fn start_loaded(self: &Arc<Self>, updated: bool, resume_path: &std::path::Path) {
        if updated {
            if let Some(resume) = Self::take_update_resume(resume_path) {
                self.resume_after_update(&resume);
                return;
            }
        }
        let _ = std::fs::remove_file(resume_path);
        let launch: Vec<String> = self.st.lock().queues.iter().filter(|q| q.start_on_launch).map(|q| q.name.clone()).collect();
        for q in launch {
            self.start_queue(&q);
        }
    }

    fn take_update_resume(path: &std::path::Path) -> Option<UpdateResume> {
        let resume = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        std::fs::remove_file(path).ok()?;
        Some(resume)
    }

    pub fn update_resume(&self) -> UpdateResume {
        let running: HashSet<String> = self.running.lock().keys().cloned().collect();
        let st = self.st.lock();
        UpdateResume {
            items: st.items.iter().filter(|i| running.contains(&i.id) || i.is_active() || i.status == DlStatus::Queued
                || (i.is_torrent && i.status == DlStatus::Completed && self.torrents.find(&i.info_hash).is_some_and(|h| !h.is_paused())))
                .map(|i| (i.id.clone(), i.status)).collect(),
            queues: st.queues.iter().filter(|q| q.running).map(|q| (q.name.clone(), q.started_ids.clone())).collect(),
        }
    }

    fn apply_update_resume(st: &mut State, resume: &UpdateResume) -> Vec<String> {
        for q in &mut st.queues {
            q.running = resume.queues.iter().any(|(name, _)| name == &q.name);
            q.started_ids = resume.queues.iter().find(|(name, _)| name == &q.name).map(|(_, ids)| ids.clone()).unwrap_or_default();
        }
        let mut start = vec![];
        for (id, status) in &resume.items {
            if let Some(it) = st.items.iter_mut().find(|i| &i.id == id) {
                if *status == DlStatus::Queued {
                    it.status = DlStatus::Queued;
                } else if it.status != DlStatus::Completed || *status == DlStatus::Completed {
                    start.push(id.clone());
                }
            }
        }
        start
    }

    pub fn resume_after_update(self: &Arc<Self>, resume: &UpdateResume) {
        let start = Self::apply_update_resume(&mut self.st.lock(), resume);
        for id in start { self.start(&id); }
        self.pump_queues();
        self.save();
        self.push_list();
    }

    pub fn save(&self) {
        let _ = self.save_checked();
    }

    pub fn save_checked(&self) -> std::io::Result<()> {
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
        match &r {
            Ok(()) => self.dirty.store(false, Ordering::Relaxed),
            Err(e) => crate::util::log_error(&format!("Save failed: {e}")),
        }
        r
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
        let policy = self_id.and_then(|id| st.items.iter().find(|it| it.id == id)).map(|it| it.existing_file_policy.as_str()).filter(|s| !s.is_empty()).unwrap_or(&st.settings.existing_file_policy);
        if policy != "Number" {
            let clean = net::sanitize(name);
            let full = std::path::Path::new(folder).join(&clean).to_string_lossy().to_lowercase();
            if !clean.is_empty() && !st.items.iter().chain(pending).any(|o| Some(o.id.as_str()) != self_id && o.status != DlStatus::Completed && o.full_path().to_lowercase() == full) { return clean; }
        }
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
            torrent_skip_patterns: if skipped.is_none() { s.torrent_skip_patterns.clone() } else { String::new() },
            torrent_mark_unfinished: s.torrent_mark_unfinished,
            extra_trackers: crate::torrent::tracker_lines(&s.torrent_always_trackers).unwrap_or_default(),
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
        let mut ids = vec![];
        let mut duplicates = vec![];
        {
            let mut st = self.st.lock();
            for mut it in items {
                if it.existing_file_policy.is_empty() { it.existing_file_policy = st.settings.existing_file_policy.clone(); }
                if !it.is_video && it.incomplete_folder.is_empty() && !st.settings.incomplete_folder.trim().is_empty() {
                    it.incomplete_folder = std::path::Path::new(&st.settings.incomplete_folder).join(&it.id).to_string_lossy().into_owned();
                }
                if it.is_torrent {
                    if it.info_hash.is_empty() {
                        it.info_hash = crate::torrent::source_hash(&it.url).unwrap_or_default();
                    }
                    if let Some(existing) = st.items.iter().find(|o| Self::same_torrent(o, &it.info_hash)) {
                        duplicates.push(existing.clone());
                        continue;
                    }
                }
                it.status = if !start && !it.queue_name.is_empty() { DlStatus::Queued } else { DlStatus::Paused };
                ids.push(it.id.clone());
                st.items.insert(0, it);
            }
        }
        for it in duplicates { self.torrent_duplicate(&it); }
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

    fn same_torrent(it: &Item, hash: &str) -> bool {
        it.is_torrent && !hash.is_empty() && (it.info_hash.eq_ignore_ascii_case(hash)
            || (it.info_hash.is_empty() && crate::torrent::source_hash(&it.url).is_some_and(|h| h.eq_ignore_ascii_case(hash))))
    }

    fn torrent_duplicate(&self, it: &Item) {
        self.toast("Torrent already in the list", &format!("{} — {}", it.file_name, it.status_text()), None);
        self.emit("torrent-duplicate", json!({ "id": it.id }));
    }

    /// Claim a resolved hash under the list lock, before touching the engine or data.
    pub fn claim_torrent_hash(&self, id: &str, hash: &str) -> bool {
        let duplicate = {
            let mut st = self.st.lock();
            let duplicate = st.items.iter().find(|i| i.id != id && Self::same_torrent(i, hash)).cloned();
            if duplicate.is_some() {
                st.items.retain(|i| i.id != id);
            } else if let Some(it) = st.items.iter_mut().find(|i| i.id == id) {
                it.info_hash = hash.to_string();
            } else { return false; }
            duplicate
        };
        if let Some(it) = duplicate {
            self.torrent_duplicate(&it);
            false
        } else { true }
    }

    // ------------------------------------------------------------------ start / stop

    pub fn start(self: &Arc<Self>, id: &str) {
        let guard = if self.item(id).is_some_and(|i| i.is_torrent) {
            match self.torrents.operation(id).try_lock_owned() {
                Ok(g) => Some(g),
                Err(_) => {
                    self.with_item(id, |i| i.note = "An operation is still finishing; try Resume again shortly".into());
                    return;
                }
            }
        } else { None };
        self.start_inner(id, guard);
    }

    pub(crate) fn start_inner(self: &Arc<Self>, id: &str, guard: Option<tokio::sync::OwnedMutexGuard<()>>) {
        if self.exiting.load(Ordering::Relaxed) { return; }
        let (kind, limit, queue) = {
            let st = self.st.lock();
            let Some(it) = st.items.iter().find(|i| i.id == id) else { return };
            if it.status == DlStatus::Completed {
                // A finished torrent: Resume means share it again (Pause stopped that).
                if it.is_torrent {
                    let (mgr, id) = (self.clone(), id.to_string());
                    tauri::async_runtime::spawn(async move {
                        let _guard = guard;
                        mgr.torrents.resume_seeding(&mgr, &id).await;
                        mgr.push_list();
                    });
                }
                return;
            }
            (it.kind(), it.speed_limit_kbps, it.queue_name.clone())
        };
        let mut running = self.running.lock();
        if running.contains_key(id) {
            return;
        }
        {
            let mut st = self.st.lock();
            if queue.is_empty() && st.items.iter().filter(|i| i.queue_name.is_empty() && running.contains_key(&i.id)).count()
                >= st.settings.max_manual_downloads.clamp(1, 100) as usize
            {
                if let Some(it) = st.items.iter_mut().find(|i| i.id == id) {
                    it.status = DlStatus::Queued;
                    it.not_before = None;
                }
                self.mark_dirty();
                return;
            }
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
            created_at: Instant::now(),
            ended: tx,
            book: Mutex::new(SpeedBook::default()),
        });
        run.limiter.set(limit as i64 * 1024);
        running.insert(id.to_string(), run.clone());
        if let Some(q) = self.st.lock().queues.iter_mut().find(|q| q.name == queue && q.running) {
            q.started_ids.insert(id.to_string());
            q.finish_notified = false;
        }
        drop(running);
        self.refresh_limits();
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
        self.refresh_limits();
        if let Some(it) = self.item(&run.id).filter(|i| i.existing_file_policy == "Ask" && i.status == DlStatus::Paused && i.error_message.contains("file conflict dialog")) {
            self.emit("file-conflict", json!({ "id": it.id, "name": it.file_name }));
        }
        let settings = self.settings();
        let mut completed: Option<Item> = None;
        let mut failed: Option<Item> = None;
        let mut retrying: Option<Item> = None;
        {
            let mut st = self.st.lock();
            let retries: HashMap<String, (i32, i32)> = st.queues.iter().filter(|q| q.running)
                .map(|q| (q.name.clone(), (q.retry_count.clamp(0, 100), q.retry_delay_sec.clamp(1, 86400)))).collect();
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
                    if let Some((_, delay)) = retries.get(&it.queue_name).filter(|(count, _)| it.retry_count < *count) {
                        it.retry_count += 1;
                        it.not_before = Some(Local::now() + chrono::Duration::seconds(*delay as i64));
                        it.status = DlStatus::Queued;
                        retrying = Some(it.clone());
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
                self.item_toast(&it, "finished", String::new());
            }
            crate::notify::finished(&settings, &it);
            if it.open_when_done {
                win::open(&it.full_path());
            }
            crate::covers::refresh_ids(self, std::slice::from_ref(&it.id));
        }
        if let Some(it) = failed {
            if !self.exiting.load(Ordering::Relaxed) { crate::notify::failed(&settings, &it); }
            if settings.notify_on_complete && !self.exiting.load(Ordering::Relaxed) {
                // Kept short and on two lines: a long name or error text overflowed the small notification card.
                let clip = |s: &str, n: usize| -> String {
                    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
                    if one.chars().count() > n { format!("{}…", one.chars().take(n).collect::<String>().trim_end()) } else { one }
                };
                let why = if it.error_message.trim().is_empty() { "Unknown error".to_string() } else { clip(&it.error_message, 85) };
                self.item_toast(&it, "error", format!("{}\n{}", clip(&it.file_name, 60), why));
            }
        }
        if let Some(it) = retrying {
            if settings.notify_on_complete && !self.exiting.load(Ordering::Relaxed) {
                let delay = it.not_before.map(|t| (t - Local::now()).num_seconds().max(1)).unwrap_or(1);
                self.item_toast(&it, "retrying", format!("{} - retry {} in {} s. {}", it.file_name, it.retry_count, delay, it.error_message));
            }
        }
        self.save();
        self.pump_queues();
        self.push_list();
    }

    /// Stops a download and waits until it has fully let go (so an immediate start isn't ignored).
    pub async fn pause(self: &Arc<Self>, id: &str) {
        self.torrents.cancel_check(id);
        let _guard = if self.item(id).is_some_and(|i| i.is_torrent) {
            Some(self.torrents.operation(id).lock_owned().await)
        } else { None };
        self.pause_inner(id).await;
    }

    pub(crate) async fn pause_inner(self: &Arc<Self>, id: &str) {
        if let Some(run) = self.run_of(id) {
            {
                // Serialize a pause with the runner's final completion step.
                let _st = self.st.lock();
                run.cancel.cancel();
            }
            let mut rx = run.ended.subscribe();
            if run.kind == Kind::Torrent {
                let _ = rx.wait_for(|v| *v).await;
            } else {
                let _ = tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|v| *v)).await;
            }
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
            for it in &mut st.items {
                if it.status == DlStatus::Queued { it.status = DlStatus::Paused; }
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
        if it.is_video {
            if self.is_running(id) { return self.warn_toast("Couldn't remove video", "The video process is still stopping. Try again shortly."); }
            if delete_file || it.status != DlStatus::Completed {
                if let Err(e) = crate::video::cleanup(&it, it.status == DlStatus::Completed) { return self.warn_toast("Couldn't delete video files", &e.to_string()); }
            }
        }
        self.st.lock().items.retain(|i| i.id != id);
        if it.is_torrent {
            // Unfinished torrents lose their partial data; finished files stay unless asked to delete them.
            self.torrents.remove(self, &it, it.status != DlStatus::Completed).await;
            let still_used = self.st.lock().items.iter().any(|o| o.torrent_file == it.torrent_file);
            if !still_used && !it.torrent_file.is_empty() {
                let _ = std::fs::remove_file(&it.torrent_file);
            }
        } else if !it.is_video {
            let _ = std::fs::remove_file(it.part_path());
        }
        if delete_file && it.status == DlStatus::Completed && !it.is_video {
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
        if it.is_video {
            if self.is_running(id) { return self.warn_toast("Couldn't restart video", "The video process is still stopping. Try again shortly."); }
            if let Err(e) = crate::video::cleanup(&it, false) {
                self.warn_toast("Couldn't restart video", &e.to_string());
                return;
            }
        }
        let _ = std::fs::remove_file(it.part_path());
        self.with_item(id, |it| {
            it.segments.lock().clear();
            it.downloaded = 0;
            it.retry_count = 0;
            it.verification_pending = false;
            it.video_files.clear();
            it.video_work_folder.clear();
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
            let Some(q) = st.queues.iter().find(|q| q.name == name) else { return };
            if q.running { return; }
            let started_ids = running.iter().filter(|id| st.items.iter().any(|i| &i.id == *id && i.queue_name == name)).cloned().collect();
            for it in st.items.iter_mut() {
                if it.queue_name == name && !running.contains(&it.id) && matches!(it.status, DlStatus::Paused | DlStatus::Error) {
                    it.status = DlStatus::Queued;
                    it.retry_count = 0;
                    it.not_before = None;
                }
            }
            if let Some(q) = st.queues.iter_mut().find(|q| q.name == name) {
                q.started_ids = started_ids;
                q.finish_notified = false;
                q.schedule_slot = None;
                q.running = true;
                self.mark_dirty();
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
                q.schedule_slot = None;
                self.mark_dirty();
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

    pub fn rename_queue(&self, old: &str, new: &str) -> Result<(), String> {
        let mut st = self.st.lock();
        if new.trim().is_empty() { return Err("A queue needs a name.".into()); }
        if st.queues.iter().any(|q| q.name != old && q.name.eq_ignore_ascii_case(new)) {
            return Err("Another queue already has that name.".into());
        }
        if !st.queues.iter().any(|q| q.name == old) { return Err("That queue no longer exists.".into()); }
        if old == new { return Ok(()); }
        // Keep existing consumers attached to the same token bucket.
        { let mut limiters = self.queue_limiters.lock();
          if let Some(limiter) = limiters.remove(old) { limiters.insert(new.to_owned(), limiter); } }
        if st.settings.last_add_queue == old { st.settings.last_add_queue = new.to_string(); }
        for it in st.items.iter_mut().filter(|i| i.queue_name == old) {
            it.queue_name = new.to_string();
        }
        if let Some(q) = st.queues.iter_mut().find(|q| q.name == old) {
            q.name = new.to_string();
        }
        Ok(())
    }

    pub fn delete_queue(self: &Arc<Self>, name: &str) {
        self.stop_queue(name);
        let mut st = self.st.lock();
        for it in st.items.iter_mut().filter(|i| i.queue_name == name) {
            it.queue_name.clear();
        }
        st.queues.retain(|q| q.name != name);
        if st.settings.last_add_queue == name { st.settings.last_add_queue.clear(); }
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
                let max = st.queues.iter().find(|q| q.name == name).map(|q| q.max_concurrent.clamp(1, 20)).unwrap_or(2);
                let mut active = members.iter().filter(|id| running.contains(*id)).count() as i32;
                for id in &members {
                    if active >= max {
                        break;
                    }
                    let Some(it) = st.items.iter().find(|i| &i.id == id) else { continue };
                    if it.status == DlStatus::Queued && !running.contains(id) && it.not_before.is_none_or(|t| t <= now) {
                        to_start.push(id.clone());
                        active += 1;
                    }
                }
                let any_queued = members.iter().any(|id| st.items.iter().any(|i| &i.id == id && i.status == DlStatus::Queued));
                let all_completed = Self::queue_completed(&st, &name);
                let failures = st.queues.iter().find(|q| q.name == name).map(|q|
                    st.items.iter().filter(|i| q.started_ids.contains(&i.id) && i.status == DlStatus::Error).count()).unwrap_or(0);
                if let Some(q) = st.queues.iter_mut().find(|q| q.name == name) {
                    if active == 0 && !any_queued {
                        if (all_completed || failures > 0) && !q.finish_notified {
                            finished.push((q.clone(), failures));
                            q.finish_notified = true;
                            self.mark_dirty();
                        }
                        if q.stop_when_empty {
                            q.running = false;
                            self.mark_dirty();
                        }
                    }
                }
            }
            let active = st.items.iter().filter(|i| i.queue_name.is_empty() && running.contains(&i.id)).count();
            let slots = (st.settings.max_manual_downloads.clamp(1, 100) as usize).saturating_sub(active);
            let mut waiting: Vec<&Item> = st.items.iter().filter(|i| i.queue_name.is_empty()
                && i.status == DlStatus::Queued && !running.contains(&i.id) && i.not_before.is_none_or(|t| t <= now)).collect();
            waiting.sort_by_key(|i| i.queue_order);
            to_start.extend(waiting.into_iter().take(slots).map(|i| i.id.clone()));
        }
        for id in to_start {
            self.start(&id);
        }
        for (q, failures) in finished {
            self.on_queue_finished(q, failures);
        }
    }

    fn on_queue_finished(self: &Arc<Self>, q: QueueDef, failures: usize) {
        let s = self.settings();
        let done = self.st.lock().items.iter().filter(|i| q.started_ids.contains(&i.id) && i.status == DlStatus::Completed).count();
        crate::notify::queue_finished(&s, &q.name, done, failures);
        if failures > 0 {
            if s.notify_on_complete { self.toast("Queue stopped", &format!("Queue {} stopped: {} failed", q.name, failures), None); }
            return;
        }
        if s.notify_on_complete {
            self.toast("Queue finished", &format!("Everything in “{}” is done.", q.name), None);
        }
        if q.post_action != PostAction::None {
            if self.has_pending_downloads() {
                self.toast("Finish action skipped", "Other downloads are still running or waiting. The finish action was skipped.", None);
                return;
            }
            self.save();
            self.emit("countdown", json!({ "action": format!("{:?}", q.post_action), "force": q.force_shutdown, "queue": q.name }));
        }
    }

    pub fn has_pending_downloads(&self) -> bool {
        if !self.running.lock().is_empty() { return true; }
        let st = self.st.lock();
        st.items.iter().any(|i| i.status.is_active() || (i.status == DlStatus::Queued
            && (i.queue_name.is_empty() || st.queues.iter().any(|q| q.name == i.queue_name && q.running))))
    }

    fn queue_completed(st: &State, name: &str) -> bool {
        let Some(q) = st.queues.iter().find(|q| q.name == name) else { return false };
        !q.started_ids.is_empty() && st.items.iter().filter(|i| q.started_ids.contains(&i.id)).all(|i| i.status == DlStatus::Completed)
    }

    fn run_scheduler(self: &Arc<Self>) {
        let now = Local::now();
        let wall = now.naive_local();
        let previous = self.last_scheduler.lock().replace(wall);
        let mut start = vec![];
        let mut stop = vec![];
        {
            let mut st = self.st.lock();
            for q in &mut st.queues {
                if q.running {
                    if let Some(day) = scheduled_stop(q, wall, previous) {
                        q.last_stop_slot = Some(day);
                        q.last_stop_fire = now;
                        stop.push(q.name.clone());
                    }
                } else if let Some(day) = scheduled_start(q, wall) {
                    q.last_start_slot = Some(day);
                    q.last_start_fire = now;
                    start.push((q.name.clone(), day));
                }
            }
        }
        for (name, day) in &start {
            self.start_queue(name);
            if let Some(q) = self.st.lock().queues.iter_mut().find(|q| &q.name == name) {
                q.schedule_slot = Some(*day);
            }
        }
        for name in &stop { self.stop_queue(name); }
        if !start.is_empty() || !stop.is_empty() {
            // Persist the slot guard immediately, including an overnight slot's starting date.
            self.save();
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
        if tick.is_multiple_of(60) && !self.exiting.load(Ordering::Relaxed) {
            crate::notify::tick(self);
        }
        if tick.is_multiple_of(4) && !self.exiting.load(Ordering::Relaxed) {
            if let Ok(mut watcher) = self.watched.clone().try_lock_owned() {
                let mgr = self.clone();
                tauri::async_runtime::spawn(async move { watcher.scan(&mgr, Instant::now(), true).await; });
            }
        }
        let now = Instant::now();
        let settings = self.settings();
        self.refresh_limits();
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
                // yt-dlp fixes its limit at start: reconnect for substantial, infrequent changes. Torrents take new
                // limits live (torrent.rs tick); restarting one would re-check its data on disk.
                if run.kind == Kind::Video && !run.restart_requested.load(Ordering::Relaxed) {
                    let started = run.started_limit.load(Ordering::Relaxed);
                    if started >= 0 && run.active.load(Ordering::Relaxed) > 0 {
                        let cur = run.limiter.get();
                        let changed = video_budget_changed(started, cur, run.created_at.elapsed());
                        if changed {
                            run.restart_requested.store(true, Ordering::Relaxed);
                            restarts.push(run.id.clone());
                        }
                    }
                }
            }
            for it in st.items.iter_mut() {
                if !runs.iter().any(|r| r.id == it.id) && (!it.speed_history.is_empty() || it.seeding) {
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
            "seeding": st.items.iter().filter(|i| i.seeding).count(),
            "upSpeedText": fmt::speed(st.items.iter().filter(|i| i.is_torrent).map(|i| i.upload_speed).sum()),
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
    let active = it.is_active() || it.seeding;
    let eta = if downloading && it.speed > 1.0 && it.total_size > 0 { fmt::eta((it.total_size - it.downloaded) as f64 / it.speed) } else { String::new() };
    let progress_text = if it.status == DlStatus::Completed || it.total_size > 0 {
        format!("{:.1}%", it.progress())
    } else if it.downloaded > 0 {
        fmt::size(it.downloaded)
    } else {
        String::new()
    };
    let conn = if it.is_torrent {
        if active { format!("{} peers", it.connected_peers) } else { String::new() }
    } else if downloading {
        format!("{}/{}", it.active_connections, it.connections)
    } else {
        String::new()
    };
    let ratio = if !it.is_torrent || it.downloaded <= 0 { String::new() } else if it.ratio() >= 9999.0 { "∞".into() } else { format!("{:.2}", it.ratio()) };
    let icon = crate::covers::icon_url(it, s);
    json!({
        "id": it.id,
        "name": it.file_name, "title": crate::covers::display_title(it),
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
        "completed": it.completed_at.map(|t| t.format("%b %d %H:%M").to_string()),
        "torrent": it.is_torrent,
        "seeding": it.seeding,
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
        "game": crate::covers::is_game(it), "artColor": it.art_color, "banner": crate::covers::banner_url(it, s),
        "throttled": it.throttled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(value: &str) -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn scheduler_catches_up_only_inside_the_selected_slot() {
        let mut q = QueueDef { start_at_enabled: true, stop_at_enabled: true, days_mask: 1 << 1, ..Default::default() };
        let monday = wall("2026-11-02 02:00").date();
        for time in ["2026-11-02 02:00", "2026-11-02 04:40", "2026-11-02 07:59"] {
            assert_eq!(scheduled_start(&q, wall(time)), Some(monday));
        }
        for time in ["2026-11-02 01:59", "2026-11-02 08:00", "2026-11-02 18:00", "2026-11-03 04:00"] {
            assert_eq!(scheduled_start(&q, wall(time)), None);
        }
        q.last_start_slot = Some(monday);
        q = serde_json::from_value(serde_json::to_value(q).unwrap()).unwrap();
        assert_eq!(scheduled_start(&q, wall("2026-11-02 05:00")), None);
        q.last_start_slot = None;
        q.last_start_fire = parse_when("2026-11-02T02:00:00").unwrap();
        assert_eq!(scheduled_start(&q, wall("2026-11-02 05:00")), None);
        q.last_start_fire = min_when();
        q.stop_at_enabled = false;
        assert_eq!(scheduled_start(&q, wall("2026-11-02 23:59")), Some(monday));
        assert_eq!(scheduled_start(&q, wall("2026-11-03 00:01")), Some(monday));
        assert_eq!(scheduled_start(&q, wall("2026-11-03 02:00")), None);
        q.start_at = "invalid".into();
        assert_eq!(scheduled_start(&q, wall("2026-11-02 03:00")), None);
    }

    #[test]
    fn scheduler_overnight_slots_use_the_start_day_and_once_date() {
        let mut q = QueueDef { start_at_enabled: true, start_at: "22:00".into(), stop_at_enabled: true,
            stop_at: "02:00".into(), days_mask: 1 << 1, ..Default::default() };
        let monday = wall("2026-11-02 22:00").date();
        assert_eq!(scheduled_start(&q, wall("2026-11-03 01:59")), Some(monday));
        assert_eq!(scheduled_start(&q, wall("2026-11-03 02:00")), None);
        assert_eq!(scheduled_start(&q, wall("2026-11-03 22:00")), None);
        q.daily = false;
        q.once_date = parse_when("2026-11-02T00:00:00").unwrap();
        assert_eq!(scheduled_start(&q, wall("2026-11-03 01:00")), Some(monday));
        q.last_start_slot = Some(monday);
        assert_eq!(scheduled_start(&q, wall("2026-11-03 01:00")), None);
        q.running = true;
        q.schedule_slot = Some(monday);
        q = serde_json::from_value(serde_json::to_value(q).unwrap()).unwrap();
        assert_eq!(scheduled_stop(&q, wall("2026-11-03 04:00"), Some(wall("2026-11-02 23:00"))), Some(monday.succ_opt().unwrap()));
        q.last_stop_slot = Some(monday.succ_opt().unwrap());
        assert_eq!(scheduled_stop(&q, wall("2026-11-03 04:01"), Some(wall("2026-11-02 23:00"))), None);
    }

    #[test]
    fn scheduler_dst_gaps_repeated_hours_and_sleep_do_not_duplicate_slots() {
        let mut q = QueueDef { start_at_enabled: true, start_at: "02:30".into(), stop_at_enabled: true, ..Default::default() };
        // A missing wall-clock hour catches up as soon as the clock advances into the window.
        let spring = wall("2026-03-29 03:00");
        assert_eq!(scheduled_start(&q, spring), Some(spring.date()));
        q.last_start_slot = Some(spring.date());
        assert_eq!(scheduled_start(&q, wall("2026-03-29 03:30")), None);
        let autumn = wall("2026-10-25 02:45");
        assert_eq!(scheduled_start(&q, autumn), Some(autumn.date()));
        q.last_start_slot = Some(autumn.date());
        assert_eq!(scheduled_start(&q, wall("2026-10-25 02:30")), None);
        assert_eq!(scheduled_start(&q, wall("2026-10-25 02:59")), None);
        q.schedule_slot = Some(autumn.date());
        assert_eq!(scheduled_stop(&q, wall("2026-10-26 11:00"), Some(autumn)), Some(wall("2026-10-26 08:00").date()));
        q.schedule_slot = None;
        // A queue manually started after a missed stop time waits for the next stop.
        assert_eq!(scheduled_stop(&q, wall("2026-10-26 11:01"), Some(wall("2026-10-26 11:00"))), None);
    }

    #[test]
    fn queue_idle_policy_and_completion_events_are_consistent() {
        let mgr = Manager::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        mgr.set_ui(Box::new(move |event, _| captured.lock().push(event.to_string())));
        let done = Item { queue_name: "q".into(), status: DlStatus::Completed, ..Default::default() };
        {
            let mut st = mgr.st.lock();
            st.queues = vec![QueueDef { name: "q".into(), running: true, stop_when_empty: false,
                started_ids: HashSet::from([done.id.clone()]), ..Default::default() }];
            st.items = vec![done];
        }
        mgr.pump_queues();
        mgr.pump_queues();
        assert!(mgr.st.lock().queues[0].running);
        assert!(mgr.st.lock().queues[0].finish_notified);
        let saved = serde_json::to_value(&mgr.st.lock().queues[0]).unwrap();
        let restored: QueueDef = serde_json::from_value(saved).unwrap();
        assert!(restored.finish_notified);
        assert!(!restored.running);
        assert_eq!(events.lock().iter().filter(|e| *e == "toast").count(), 1);
        mgr.start_queue("q");
        assert!(mgr.st.lock().queues[0].finish_notified);
        mgr.st.lock().queues[0].stop_when_empty = true;
        mgr.pump_queues();
        assert!(!mgr.st.lock().queues[0].running);
        assert_eq!(events.lock().iter().filter(|e| *e == "toast").count(), 1);
        mgr.start_queue("missing");
        assert!(mgr.st.lock().items.iter().all(|i| i.status == DlStatus::Completed));
        mgr.st.lock().items.clear();
        mgr.start_queue("q");
        assert!(!mgr.st.lock().queues[0].running);
        assert_eq!(events.lock().iter().filter(|e| *e == "toast").count(), 1);
    }

    #[test]
    fn queue_retries_are_opt_in_bounded_delayed_and_preserve_progress() {
        let mgr = Manager::new();
        let it = Item { queue_name: "q".into(), status: DlStatus::Error, downloaded: 1234, ..Default::default() };
        let run = Run::test_run(&it.id);
        {
            let mut st = mgr.st.lock();
            st.settings.notify_on_complete = false;
            st.queues = vec![QueueDef { name: "q".into(), running: true, stop_when_empty: false, ..Default::default() }];
            st.items = vec![it.clone()];
        }
        mgr.on_ended(&run);
        assert_eq!(mgr.item(&it.id).unwrap().status, DlStatus::Error);
        mgr.st.lock().queues[0].retry_count = 2;
        mgr.st.lock().queues[0].retry_delay_sec = 120;
        for attempt in 1..=2 {
            let before = Local::now();
            mgr.on_ended(&run);
            let retried = mgr.item(&it.id).unwrap();
            assert_eq!(retried.status, DlStatus::Queued);
            assert_eq!(retried.retry_count, attempt);
            assert_eq!(retried.downloaded, 1234);
            assert!(retried.not_before.unwrap() >= before + chrono::Duration::seconds(120));
            assert!(!mgr.is_running(&it.id));
            mgr.with_item(&it.id, |i| i.status = DlStatus::Error);
        }
        mgr.on_ended(&run);
        assert_eq!(mgr.item(&it.id).unwrap().status, DlStatus::Error);
        assert_eq!(mgr.item(&it.id).unwrap().retry_count, 2);
        mgr.st.lock().queues[0].running = false;
        mgr.with_item(&it.id, |i| i.retry_count = 0);
        mgr.on_ended(&run);
        assert_eq!(mgr.item(&it.id).unwrap().status, DlStatus::Error);
        mgr.st.lock().queues[0].running = true;
        mgr.with_item(&it.id, |i| i.status = DlStatus::Paused);
        mgr.on_ended(&run);
        assert_eq!(mgr.item(&it.id).unwrap().status, DlStatus::Paused);
    }

    #[test]
    fn remembered_add_queue_honours_explicit_context_and_deleted_queues() {
        let mgr = Manager::new();
        mgr.st.lock().queues = vec![QueueDef { name: "q".into(), ..Default::default() }, QueueDef::default()];
        mgr.remember_add_queue("q");
        assert_eq!(mgr.add_queue_choice(None), "q");
        assert_eq!(mgr.add_queue_choice(Some("Main queue")), "Main queue");
        assert_eq!(mgr.add_queue_choice(Some("")), "");
        mgr.rename_queue("q", "renamed").unwrap();
        assert_eq!(mgr.add_queue_choice(None), "renamed");
        let saved = serde_json::to_value(mgr.settings()).unwrap();
        let settings: Settings = serde_json::from_value(saved).unwrap();
        assert_eq!(settings.last_add_queue, "renamed");
        mgr.delete_queue("renamed");
        assert_eq!(mgr.add_queue_choice(None), "");
        mgr.remember_add_queue("missing");
        assert_eq!(mgr.add_queue_choice(None), "");
        let q: QueueDef = serde_json::from_value(json!({"Name": "old"})).unwrap();
        assert!(q.stop_when_empty);
        assert_eq!(q.retry_count, 0);
        assert_eq!(q.retry_delay_sec, 20);
        assert_eq!(Settings::default().max_manual_downloads, 3);
    }

    #[test]
    fn legacy_retry_preferences_migrate_without_enabling_new_queues() {
        let old = json!({"Name": "old"});
        let explicit = json!({"Name": "new", "RetryCount": 0, "RetryDelaySec": 45});
        let queues = load_queue_defs(vec![old.clone(), explicit], Some(4));
        assert_eq!(queues[0].retry_count, 4);
        assert_eq!(queues[0].retry_delay_sec, 20);
        assert_eq!(queues[1].retry_count, 0);
        assert_eq!(queues[1].retry_delay_sec, 45);
        assert_eq!(load_queue_defs(vec![old], None)[0].retry_count, 0);
        assert_eq!(QueueDef::default().retry_count, 0);
    }

    #[tokio::test]
    async fn enabled_empty_queue_starts_later_additions_and_stop_cancels_retry_waits() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mgr = Manager::new();
        {
            let mut st = mgr.st.lock();
            st.settings.lookup_covers_online = false;
            st.settings.notify_on_complete = false;
            st.queues = vec![QueueDef { name: "q".into(), stop_when_empty: false, ..Default::default() }];
        }
        mgr.start_queue("q");
        assert!(mgr.st.lock().queues[0].running);
        let it = Item { url: format!("http://{}/file", listener.local_addr().unwrap()), queue_name: "q".into(), ..Default::default() };
        mgr.add(vec![it.clone()], false);
        assert!(mgr.is_running(&it.id));
        assert!(mgr.st.lock().queues[0].started_ids.contains(&it.id));
        let delayed = Item { queue_name: "q".into(), status: DlStatus::Queued,
            not_before: Some(Local::now() + chrono::Duration::seconds(60)), ..Default::default() };
        mgr.st.lock().items.push(delayed.clone());
        mgr.stop_queue("q");
        assert!(!mgr.st.lock().queues[0].running);
        assert_eq!(mgr.item(&delayed.id).unwrap().status, DlStatus::Paused);
        mgr.pause(&it.id).await;
        assert!(!mgr.is_running(&it.id));
        mgr.pump_queues();
        assert!(!mgr.is_running(&delayed.id));
    }

    #[tokio::test]
    async fn manual_limit_queues_fifo_across_engines_and_pause_cancels_waiters() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let mgr = Manager::new();
        let mut ids = vec![];
        {
            let mut st = mgr.st.lock();
            st.settings.max_manual_downloads = 3;
            st.settings.lookup_covers_online = false;
            for order in 0..5 {
                let it = Item { url: url.clone(), queue_order: order, ..Default::default() };
                ids.push(it.id.clone());
                st.items.insert(0, it);
            }
        }
        for (id, kind) in ids.iter().take(3).zip([Kind::Http, Kind::Torrent, Kind::Video]) {
            let mut run = Run::test_run(id);
            Arc::get_mut(&mut run).unwrap().kind = kind;
            mgr.running.lock().insert(id.clone(), run);
        }
        mgr.start(&ids[3]);
        mgr.start(&ids[4]);
        assert_eq!(mgr.item(&ids[3]).unwrap().status, DlStatus::Queued);
        assert_eq!(mgr.item(&ids[4]).unwrap().status, DlStatus::Queued);
        let ended = mgr.run_of(&ids[0]).unwrap();
        mgr.with_item(&ids[0], |i| i.status = DlStatus::Completed);
        mgr.on_ended(&ended);
        assert!(mgr.is_running(&ids[3]));
        assert!(!mgr.is_running(&ids[4]));
        mgr.pause(&ids[4]).await;
        assert_eq!(mgr.item(&ids[4]).unwrap().status, DlStatus::Paused);
        mgr.with_item(&ids[4], |i| i.status = DlStatus::Queued);
        mgr.running.lock().remove(&ids[1]);
        mgr.running.lock().remove(&ids[2]);
        mgr.pause_all().await;
        assert!(!mgr.is_running(&ids[3]));
        assert!(!mgr.is_running(&ids[4]));
        assert_eq!(mgr.item(&ids[4]).unwrap().status, DlStatus::Paused);
        drop(listener);
    }

    #[test]
    fn mixed_engines_share_global_and_queue_budgets_and_rebalance() {
        let mgr = Manager::new();
        let items: Vec<Item> = (0..4).map(|n| Item { id: n.to_string(), queue_name: if n < 3 { "q".into() } else { String::new() }, speed_limit_kbps: if n == 2 { 100 } else { 0 }, ..Default::default() }).collect();
        {
            let mut st = mgr.st.lock();
            st.items = items.clone();
            st.settings.global_speed_limit_kbps = 1200;
            st.queues = vec![QueueDef { name: "q".into(), speed_limit_kbps: 600, ..Default::default() }];
        }
        for (it, kind) in items.iter().zip([Kind::Http, Kind::Torrent, Kind::Video, Kind::Video]) {
            let mut run = Run::test_run(&it.id);
            Arc::get_mut(&mut run).unwrap().kind = kind;
            mgr.running.lock().insert(it.id.clone(), run);
        }
        mgr.refresh_limits();
        let limits = || (0..4).filter_map(|n| mgr.run_of(&n.to_string()).map(|r| r.limiter.get() / 1024)).collect::<Vec<_>>();
        assert_eq!(limits(), [250, 250, 100, 600]);
        mgr.running.lock().remove("2");
        mgr.refresh_limits();
        assert_eq!(limits(), [300, 300, 600]);
        mgr.st.lock().settings.global_speed_limit_kbps = 0;
        mgr.refresh_limits();
        assert_eq!(limits(), [300, 300, 0]);
        assert_eq!(download_allocations(&[0; 10], &[0; 10], &[0], 1024), vec![102; 10]);
        assert_eq!(download_allocations(&[0; 4], &[0; 4], &[1024], 0), vec![256; 4]);
    }

    #[test]
    fn idle_torrent_returns_capacity_to_http_and_busy_consumers_split_evenly() {
        let mgr = Manager::new();
        mgr.st.lock().settings.global_speed_limit_kbps = 1024;
        for (id, kind, speed) in [("http", Kind::Http, 1024.0 * 1024.0), ("torrent", Kind::Torrent, 0.0)] {
            mgr.st.lock().items.push(Item { id: id.into(), ..Default::default() });
            let mut run = Run::test_run(id);
            Arc::get_mut(&mut run).unwrap().kind = kind;
            *run.book.lock() = SpeedBook { smooth: speed, last_t: Some(Instant::now()), ..Default::default() };
            mgr.running.lock().insert(id.into(), run);
        }
        mgr.refresh_limits();
        assert_eq!(mgr.run_of("http").unwrap().limiter.get(), 1024 * 1024 - 4096);
        assert_eq!(mgr.run_of("torrent").unwrap().limiter.get(), 4096);
        mgr.run_of("torrent").unwrap().book.lock().smooth = 1024.0 * 1024.0;
        mgr.refresh_limits();
        assert_eq!(mgr.run_of("http").unwrap().limiter.get(), 512 * 1024);
        assert_eq!(mgr.run_of("torrent").unwrap().limiter.get(), 512 * 1024);
        assert_eq!(download_allocations(&[4096, 0, 0], &[0, 0, 1], &[512 * 1024, 0], 1024 * 1024), [4096, 512 * 1024 - 4096, 512 * 1024]);
    }

    #[test]
    fn video_budget_reconnect_requires_large_change_and_cooldown() {
        let ready = Duration::from_secs(60);
        assert!(!video_budget_changed(1000, 1200, ready));
        assert!(!video_budget_changed(1000, 700, ready));
        assert!(!video_budget_changed(1000, 1600, Duration::from_secs(59)));
        assert!(video_budget_changed(1000, 1600, ready));
        assert!(video_budget_changed(1000, 0, ready));
        assert!(video_budget_changed(0, 1000, ready));
        assert!(!video_budget_changed(-1, 1000, ready));
    }

    #[test]
    fn completed_seeding_rows_and_summary_include_upload_activity() {
        let mgr = Manager::new();
        let it = Item { is_torrent: true, status: DlStatus::Completed, seeding: true, upload_speed: 2048.0, connected_peers: 3, connected_seeds: 1, ..Default::default() };
        let row = row_json(&it, false, &Settings::default());
        assert_eq!(row["upSpeedText"], fmt::speed(2048.0));
        assert_eq!(row["conn"], "3 peers");
        assert!(!row["peers"].as_str().unwrap().is_empty());
        mgr.st.lock().items.push(it);
        let (_, _, summary) = mgr.snapshot();
        assert_eq!(summary["seeding"], 1);
        assert_eq!(summary["running"], 0);
        assert_eq!(summary["upSpeedText"], fmt::speed(2048.0));
    }

    fn state(items: Vec<Item>) -> State {
        State { items, queues: vec![QueueDef::default()], settings: Settings::default(), stats: StatsStore::default() }
    }

    #[test]
    fn queue_only_finishes_when_every_remaining_started_item_completed() {
        let done = Item { queue_name: "test".into(), status: DlStatus::Completed, ..Default::default() };
        let old = Item { queue_name: "test".into(), status: DlStatus::Error, ..Default::default() };
        let mut st = state(vec![done.clone(), old]);
        st.queues[0].name = "test".into();
        assert!(!Manager::queue_completed(&st, "test"));
        st.queues[0].started_ids.insert(done.id.clone());
        assert!(Manager::queue_completed(&st, "test"));
        assert!(!Manager::queue_completed(&st, "empty"));
        for status in [DlStatus::Paused, DlStatus::Error, DlStatus::Queued, DlStatus::Downloading, DlStatus::Verifying] {
            st.items[0].status = status;
            assert!(!Manager::queue_completed(&st, "test"));
        }
        st.items.remove(0);
        assert!(Manager::queue_completed(&st, "test"));
        st.items.clear();
        let mgr = Manager::new();
        *mgr.st.lock() = st;
        mgr.start_queue("test");
        assert!(mgr.st.lock().queues[0].started_ids.is_empty());
    }

    #[test]
    fn resolved_duplicate_keeps_existing_item_in_every_status() {
        let mgr = Manager::new();
        for status in [DlStatus::Paused, DlStatus::Error, DlStatus::Queued, DlStatus::Completed, DlStatus::Downloading] {
            let existing = Item { is_torrent: true, info_hash: "ABC123".into(), status, ..Default::default() };
            let pending = Item { is_torrent: true, ..Default::default() };
            mgr.st.lock().items = vec![existing.clone(), pending.clone()];
            assert!(!mgr.claim_torrent_hash(&pending.id, "abc123"));
            assert_eq!(mgr.st.lock().items.len(), 1);
            assert_eq!(mgr.st.lock().items[0].id, existing.id);
            assert_eq!(mgr.st.lock().items[0].status, status);
        }
    }

    #[test]
    fn update_restores_only_recorded_items_and_queue_state() {
        let active = Item::default();
        let queued = Item::default();
        let paused = Item::default();
        let mut st = state(vec![active.clone(), queued.clone(), paused.clone()]);
        let name = st.queues[0].name.clone();
        let snapshot = UpdateResume { items: vec![(active.id.clone(), DlStatus::Downloading), (queued.id.clone(), DlStatus::Queued)], queues: vec![(name, HashSet::from([active.id.clone()]))] };
        let snapshot: UpdateResume = serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        assert_eq!(Manager::apply_update_resume(&mut st, &snapshot), vec![active.id.clone()]);
        assert_eq!(st.items[1].status, DlStatus::Queued);
        assert_eq!(st.items[2].status, DlStatus::Paused);
        assert!(st.queues[0].running);
        assert_eq!(st.queues[0].started_ids, HashSet::from([active.id]));
        let mgr = Manager::new();
        *mgr.st.lock() = st;
        assert_eq!(mgr.update_resume().queues, snapshot.queues);
    }

    #[test]
    fn updated_without_readable_resume_starts_launch_queues() {
        let root = std::env::current_dir().unwrap().join("target").join(format!("startup-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("update-resume.json");
        for corrupt in [false, true] {
            if corrupt { std::fs::write(&path, b"invalid json").unwrap(); }
            let mgr = Manager::new();
            {
                let mut st = mgr.st.lock();
                *st = state(vec![]);
                st.queues[0].start_on_launch = true;
                let queue_name = st.queues[0].name.clone();
                st.items.push(Item { queue_name, status: DlStatus::Queued, not_before: Some(Local::now() + chrono::Duration::hours(1)), ..Default::default() });
            }
            mgr.start_loaded(true, &path);
            assert!(mgr.st.lock().queues[0].running);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_resume_record_is_consumed_once() {
        let path = std::env::current_dir().unwrap().join("target").join(format!("update-test-{}.json", uuid::Uuid::new_v4()));
        let resume = UpdateResume { items: vec![("running".into(), DlStatus::Downloading)], queues: vec![] };
        std::fs::write(&path, serde_json::to_vec(&resume).unwrap()).unwrap();
        assert_eq!(Manager::take_update_resume(&path).unwrap().items[0].0, "running");
        assert!(!path.exists());
        assert!(Manager::take_update_resume(&path).is_none());
    }
    #[test]
    fn edited_future_schedule_rearms_today_without_repeating_past_or_unchanged_slots() {
        let now = chrono::NaiveDateTime::parse_from_str("2026-10-30 12:00", "%Y-%m-%d %H:%M").unwrap();
        let old = QueueDef { start_at_enabled: true, start_at: "09:00".into(), daily: true,
            days_mask: 127, last_start_slot: Some(now.date()), last_start_fire: Local::now(), ..Default::default() };
        let mut saved = old.clone();
        preserve_queue_runtime(&mut saved, &old, now);
        assert_eq!(scheduled_start(&saved, now), None);
        saved.start_at = "08:00".into();
        preserve_queue_runtime(&mut saved, &old, now);
        assert_eq!(scheduled_start(&saved, now), None);
        saved.start_at = "13:00".into();
        preserve_queue_runtime(&mut saved, &old, now);
        assert_eq!(scheduled_start(&saved, now), None);
        assert_eq!(scheduled_start(&saved, now + chrono::Duration::hours(1)), Some(now.date()));
        saved.start_at = old.start_at.clone(); saved.days_mask = 1;
        preserve_queue_runtime(&mut saved, &old, now);
        assert_eq!(saved.last_start_slot, old.last_start_slot);
        saved.start_at = "14:00".into(); saved.days_mask = 127;
        preserve_queue_runtime(&mut saved, &old, now);
        assert_eq!(scheduled_start(&saved, now + chrono::Duration::hours(2)), Some(now.date()));
    }

    #[test]
    fn queue_rename_preserves_shared_limiter_and_rejects_duplicates_atomically() {
        let mgr = Manager::new();
        let item = Item { queue_name: "old".into(), ..Default::default() };
        { let mut st = mgr.st.lock(); st.items = vec![item.clone()];
          st.queues = vec![QueueDef { name: "old".into(), ..Default::default() }, QueueDef { name: "taken".into(), ..Default::default() }]; }
        let limiter = mgr.queue_limiter("old").unwrap(); limiter.set(12345);
        assert!(mgr.rename_queue("old", "TAKEN").is_err());
        assert_eq!(mgr.item(&item.id).unwrap().queue_name, "old");
        mgr.rename_queue("old", "new").unwrap();
        let renamed = mgr.queue_limiter("new").unwrap();
        assert!(Arc::ptr_eq(&limiter, &renamed)); assert_eq!(renamed.get(), 12345);
        assert!(!mgr.queue_limiters.lock().contains_key("old"));
        assert_eq!(mgr.item(&item.id).unwrap().queue_name, "new");
    }

    #[test]
    fn speed_schedule_uses_selected_start_day_across_midnight_and_roundtrips() {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.schedule_days_mask, 127); assert_eq!(s.existing_file_policy, "Number"); assert!(s.incomplete_folder.is_empty());
        s.speed_schedule_enabled = true; s.schedule_days_mask = 1 << 5; s.schedule_from = "23:00".into(); s.schedule_to = "07:00".into();
        let at = |v| chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%d %H:%M").unwrap();
        for (time, active) in [("2026-10-30 22:59", false), ("2026-10-30 23:00", true), ("2026-10-31 06:59", true), ("2026-10-31 07:00", false), ("2026-10-31 23:00", false)] {
            assert_eq!(Manager::schedule_active_at(&s, at(time)), active, "{time}");
        }
        s.schedule_days_mask = 0; assert!(!Manager::schedule_active_at(&s, at("2026-10-30 23:01")));
        s.proxy_mode = "HTTP".into(); s.proxy_host = "example.invalid".into(); s.proxy_password = "secret".into(); s.existing_file_policy = "Ask".into();
        let restored: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(restored.proxy_password, "secret"); assert_eq!(restored.existing_file_policy, "Ask"); assert_eq!(restored.schedule_days_mask, 0);
    }

    #[test]
    fn disk_full_pauses_preserves_partial_data_and_never_uses_queue_retries() {
        for e in [std::io::Error::from_raw_os_error(39), std::io::Error::from_raw_os_error(112), std::io::Error::from(std::io::ErrorKind::StorageFull)] {
            let mgr = Manager::new();
            let err = net::DlError::from(e);
            assert_eq!(err.kind, net::ErrKind::DiskFull); assert!(!err.retryable());
            let mut it = Item { queue_name: "q".into(), status: DlStatus::Downloading, downloaded: 12345, ..Default::default() };
            err.apply_download_error(&mut it);
            assert_eq!(it.status, DlStatus::Paused); assert!(it.error_message.contains("Free some space, then resume"));
            assert!(it.status_text().contains("Free some space, then resume"));
            let run = Run::test_run(&it.id);
            { let mut st = mgr.st.lock(); st.settings.notify_on_complete = false; st.items = vec![it.clone()];
              st.queues = vec![QueueDef { name: "q".into(), running: true, retry_count: 100, ..Default::default() }]; }
            mgr.on_ended(&run);
            let paused = mgr.item(&it.id).unwrap();
            assert_eq!(paused.status, DlStatus::Paused); assert_eq!(paused.retry_count, 0);
            assert_eq!(paused.downloaded, 12345); assert!(paused.not_before.is_none());
        }
    }

    #[test]
    fn failed_queue_notifies_once_without_post_action_and_success_waits_for_other_work() {
        let mgr = Manager::new(); let events = Arc::new(Mutex::new(Vec::new())); let log = events.clone();
        mgr.set_ui(Box::new(move |e, p| log.lock().push((e.to_owned(), p))));
        let it = Item { queue_name: "q".into(), status: DlStatus::Error, ..Default::default() };
        { let mut st = mgr.st.lock(); st.settings.notify_on_complete = true; st.items = vec![it.clone()];
          st.queues = vec![QueueDef { name: "q".into(), running: true, post_action: PostAction::Shutdown,
            started_ids: HashSet::from([it.id.clone()]), ..Default::default() }]; }
        mgr.pump_queues(); mgr.pump_queues();
        assert_eq!(events.lock().iter().filter(|(e, p)| e == "toast" && p["body"].as_str().unwrap_or("").contains("1 failed")).count(), 1);
        assert!(!events.lock().iter().any(|(e, _)| e == "countdown"));
        mgr.with_item(&it.id, |i| i.status = DlStatus::Completed);
        { let mut st = mgr.st.lock(); st.queues[0].running = true; st.queues[0].finish_notified = false;
          st.items.push(Item { status: DlStatus::Downloading, ..Default::default() }); }
        mgr.pump_queues();
        assert!(!events.lock().iter().any(|(e, _)| e == "countdown"));
        assert!(mgr.has_pending_downloads());
        { let mut st = mgr.st.lock(); st.items.pop(); st.queues[0].running = true; st.queues[0].finish_notified = false; }
        assert!(!mgr.has_pending_downloads()); mgr.pump_queues();
        assert_eq!(events.lock().iter().filter(|(e, _)| e == "countdown").count(), 1);
        let waiting = Item { status: DlStatus::Queued, not_before: Some(Local::now() + chrono::Duration::hours(1)), ..Default::default() };
        mgr.st.lock().items.push(waiting); assert!(mgr.has_pending_downloads());
    }

}
