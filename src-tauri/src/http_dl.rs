//! Runs one HTTP download. Resumable files are split into byte-range segments fetched in parallel and
//! written straight into a sparse ".fluxpart" file; each segment's progress is saved, so a pause, crash or
//! reboot resumes exactly where every connection stopped. When a connection finishes early it steals half
//! of the largest remaining segment, so all connections stay busy until the very end.
//!
//! Backup links (mirrors): connections are spread round-robin over every link that still works. A link that
//! fails permanently (404/403/410, wrong size, no range support…) is dropped and the others carry on; the
//! download only fails when every link is dead. Mirrors added while running are picked up live.

use crate::manager::{Manager, Run};
use crate::model::*;
use crate::net::{self, DlError, DlResult, ErrKind};
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use std::os::windows::fs::FileExt;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

const MIN_SEGMENT: i64 = 1024 * 1024;
const SAMPLE_SIZE: i64 = 32 * 1024;

#[derive(Default)]
struct Links {
    urls: Vec<String>,
    dead: Vec<bool>,
    verified: Vec<bool>,
    rr: usize,
}

struct Ctx {
    mgr: Arc<Manager>,
    run: Arc<Run>,
    id: String,
    ua: String,
    headers: String,
    max_retries: i32,
    stall: Duration,
    links: Mutex<Links>,
    probed_link: AtomicUsize,
    reference: tokio::sync::Mutex<Option<Vec<u8>>>,
    fatal: Mutex<Option<DlError>>,
    worker_count: AtomicI32,
    penalty: AtomicI32,
    connections: AtomicI32,
    total: AtomicI64,
    segs: Arc<Mutex<Vec<Seg>>>,
    had_mirrors: AtomicBool,
}

enum Flow {
    Continue,
    Retire,
}

fn cancelled(run: &Run) -> DlError {
    let _ = run;
    DlError::cancelled()
}

/// Runs `fut` unless the download is cancelled first.
async fn c<T>(run: &Run, fut: impl std::future::Future<Output = T>) -> DlResult<T> {
    tokio::select! {
        v = fut => Ok(v),
        _ = run.cancel.cancelled() => Err(cancelled(run)),
    }
}

/// A problem on this PC (disk full, folder not writable…), not with the link.
fn local_fault(e: &DlError) -> bool {
    e.fatal && matches!(e.kind, ErrKind::DiskFull | ErrKind::Io)
}

fn backoff(attempt: i32) -> Duration {
    let jitter = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis()).unwrap_or(0) as u64;
    Duration::from_secs(2f64.powi(attempt.min(5)).min(30.0) as u64) + Duration::from_millis(jitter)
}

pub async fn run(mgr: Arc<Manager>, run: Arc<Run>) {
    let id = run.id.clone();
    let Some(it) = mgr.item(&id) else { return };
    let ctx = Arc::new(Ctx::new(mgr.clone(), run.clone(), &it));
    ctx.sync_urls();
    run.current.store(ctx.segmented_done().unwrap_or(it.downloaded.max(0)), Ordering::Relaxed);

    let result = ctx.clone().go().await;
    let done = run.current.load(Ordering::Relaxed);
    match result {
        Ok(()) => {}
        Err(e) if e.kind == ErrKind::Cancelled || run.is_cancelled() => {
            mgr.with_item(&id, |it| {
                it.downloaded = done;
                it.set_status(DlStatus::Paused);
            });
        }
        Err(e) => {
            mgr.with_item(&id, |it| {
                it.downloaded = done;
                it.error_message = e.msg.clone();
                it.set_status(DlStatus::Error);
            });
        }
    }
}

impl Ctx {
    fn new(mgr: Arc<Manager>, run: Arc<Run>, it: &Item) -> Self {
        let s = mgr.settings();
        Self {
            mgr: mgr.clone(),
            run: run.clone(),
            id: it.id.clone(),
            ua: s.user_agent.clone(),
            headers: it.headers.clone(),
            max_retries: s.max_retries,
            stall: Duration::from_secs(s.read_timeout_sec.clamp(5, 600) as u64),
            links: Mutex::new(Links::default()),
            probed_link: AtomicUsize::new(0),
            reference: tokio::sync::Mutex::new(None),
            fatal: Mutex::new(None),
            worker_count: AtomicI32::new(0),
            penalty: AtomicI32::new(0),
            connections: AtomicI32::new(it.connections),
            total: AtomicI64::new(it.total_size),
            segs: it.segments.clone(),
            had_mirrors: AtomicBool::new(false),
        }
    }
    fn item_msg(&self, msg: impl Into<String>) {
        let msg = msg.into();
        self.mgr.with_item(&self.id, |it| it.error_message = msg);
    }

    fn queue(&self) -> String {
        self.mgr.with_item(&self.id, |it| it.queue_name.clone()).unwrap_or_default()
    }

    fn segmented_done(&self) -> Option<i64> {
        let segs = self.segs.lock();
        if segs.is_empty() {
            return None;
        }
        Some(segs.iter().map(|s| s.lock().downloaded).sum())
    }

    fn part_path(&self) -> String {
        self.mgr.with_item(&self.id, |it| it.part_path()).unwrap_or_default()
    }

    // ------------------------------------------------------------------ link pool

    fn sync_urls(&self) {
        let Some((url, mirrors)) = self.mgr.with_item(&self.id, |it| (it.url.clone(), it.mirrors.clone())) else { return };
        let mut l = self.links.lock();
        if l.urls.is_empty() {
            l.urls.push(url);
            l.dead.push(false);
            l.verified.push(false);
        }
        for m in mirrors {
            if !l.urls.contains(&m) {
                l.urls.push(m);
                l.dead.push(false);
                l.verified.push(false);
            }
        }
        if l.urls.len() > 1 {
            self.had_mirrors.store(true, Ordering::Relaxed);
        }
    }

    /// Next working link (round-robin), or None when all are dead.
    fn pick_url(&self) -> Option<usize> {
        self.sync_urls();
        let mut l = self.links.lock();
        let n = l.urls.len();
        for _ in 0..n {
            let i = l.rr % n;
            l.rr += 1;
            if !l.dead[i] {
                return Some(i);
            }
        }
        None
    }

    fn url_at(&self, i: usize) -> String {
        self.links.lock().urls[i].clone()
    }

    fn url_count(&self) -> usize {
        self.links.lock().urls.len()
    }

    fn first_alive(&self) -> Option<usize> {
        self.links.lock().dead.iter().position(|d| !d)
    }

    fn next_alive_after(&self, i: usize) -> Option<usize> {
        let l = self.links.lock();
        let n = l.urls.len();
        (1..=n).map(|k| (i + k) % n).find(|j| !l.dead[*j])
    }

    /// Drops a link after a permanent failure. Returns true if another working link remains.
    fn mark_dead(&self, i: usize, why: &DlError) -> bool {
        let alive = {
            let mut l = self.links.lock();
            l.dead[i] = true;
            l.dead.iter().filter(|d| !**d).count()
        };
        if alive > 0 {
            self.item_msg(format!("link {} dropped ({}) — continuing on {alive} other{}", i + 1, why.msg, if alive > 1 { "s" } else { "" }));
        }
        alive > 0
    }

    /// Before a backup link contributes bytes, make sure it really serves the same file: compare 32 KB from
    /// the start and the middle with the probed link. Same name and size is not enough, and mixing two
    /// different files would silently corrupt the result. Returns false if the link was dropped.
    async fn ensure_same_content(&self, link: usize) -> DlResult<bool> {
        {
            let l = self.links.lock();
            if link == self.probed_link.load(Ordering::Relaxed) || l.verified[link] {
                return Ok(!l.dead[link]);
            }
        }
        let mut reference = c(&self.run, self.reference.lock()).await?;
        {
            let l = self.links.lock();
            if l.verified[link] || l.dead[link] {
                return Ok(!l.dead[link]);
            }
        }
        if reference.is_none() {
            *reference = Some(self.sample(self.probed_link.load(Ordering::Relaxed)).await?);
        }
        let sample = self.sample(link).await?;
        if Some(&sample) != reference.as_ref() {
            self.mark_dead(link, &DlError::fatal("it serves different content (same size, different bytes)"));
            return Ok(false);
        }
        self.links.lock().verified[link] = true;
        Ok(true)
    }

    async fn sample(&self, link: usize) -> DlResult<Vec<u8>> {
        let total = self.total.load(Ordering::Relaxed);
        let mut out = vec![];
        let mut starts = vec![0i64, (total / 2 - SAMPLE_SIZE / 2).max(0)];
        starts.dedup();
        for start in starts {
            let end = (total - 1).min(start + SAMPLE_SIZE - 1);
            let fut = async {
                let resp = net::build(&self.url_at(link), &self.headers, &self.ua, Some((start, Some(end))), Method::GET).send().await?;
                net::ensure_ok(&resp)?;
                if resp.status() != StatusCode::PARTIAL_CONTENT {
                    return Err(DlError::fatal("The server doesn't support resuming (byte ranges)"));
                }
                Ok::<_, DlError>(resp.bytes().await?.to_vec())
            };
            let bytes = match c(&self.run, tokio::time::timeout(self.stall, fut)).await? {
                Ok(r) => r?,
                Err(_) => return Err(DlError::timeout(format!("No data for {}s", self.stall.as_secs()))),
            };
            out.extend(bytes);
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ run

    async fn go(self: Arc<Self>) -> DlResult<()> {
        self.mgr.with_item(&self.id, |it| {
            it.set_status(DlStatus::Connecting);
            it.error_message.clear();
            it.last_try = Some(chrono::Local::now());
        });
        if self.mgr.with_item(&self.id, |it| it.verification_pending && std::path::Path::new(&it.part_path()).is_file()).unwrap_or(false) {
            return self.finish().await;
        }
        for restart in 0..=3 {
            let probe = self.retry(|| self.probe_any()).await?;
            let probe = self.confirm_probe(probe).await?;
            self.apply_probe(&probe);
            let folder = self.mgr.with_item(&self.id, |it| it.save_folder.clone()).unwrap_or_default();
            std::fs::create_dir_all(&folder)?;
            self.check_disk_space(&folder)?;
            self.mgr.with_item(&self.id, |it| {
                it.set_status(DlStatus::Downloading);
                it.error_message.clear();
            });

            let result = if probe.resumable && probe.total > 0 {
                self.clone().segmented().await
            } else {
                self.single(probe.total).await
            };
            match result {
                Err(e) if e.kind == ErrKind::Changed => {
                    // All workers have stopped before discarding any bytes or validators.
                    self.segs.lock().clear();
                    std::fs::remove_file(self.part_path())?;
                    self.run.current.store(0, Ordering::Relaxed);
                    *self.fatal.lock() = None;
                    *self.reference.lock().await = None;
                    self.links.lock().verified.fill(false);
                    self.mgr.with_item(&self.id, |it| { it.downloaded = 0; it.http_validator = None; it.http_validator_url = None; });
                    if restart == 3 { return Err(DlError::fatal("The file on the server keeps changing. Try again later.")); }
                    self.item_msg("The file on the server changed; starting again from zero.");
                }
                Err(e) => return Err(e),
                Ok(()) => return self.finish().await,
            }
        }
        unreachable!()
    }

    async fn retry<T, F, Fut>(&self, op: F) -> DlResult<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = DlResult<T>>,
    {
        let mut attempt = 1;
        loop {
            match c(&self.run, op()).await? {
                Ok(v) => return Ok(v),
                Err(e) if e.retryable() && attempt <= self.max_retries => {
                    self.item_msg(format!("retry {attempt}: {}", e.msg));
                    c(&self.run, tokio::time::sleep(backoff(attempt))).await?;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Probes the main link, falling back to mirrors when it is dead or unreachable.
    async fn probe_any(&self) -> DlResult<net::Probe> {
        let timeout = self.mgr.settings().read_timeout_sec;
        let mut last = None;
        while let Some(i) = self.first_alive() {
            match net::probe(&self.url_at(i), &self.headers, &self.ua, timeout).await {
                Ok(p) => {
                    self.probed_link.store(i, Ordering::Relaxed);
                    return Ok(p);
                }
                Err(e) => {
                    if self.url_count() == 1 {
                        return Err(e);
                    }
                    if e.fatal {
                        if !self.mark_dead(i, &e) {
                            return Err(e);
                        }
                        last = Some(e);
                        continue;
                    }
                    // Transient failure: try the next link before backing off.
                    if let Some(j) = self.next_alive_after(i).filter(|j| *j != i) {
                        match net::probe(&self.url_at(j), &self.headers, &self.ua, timeout).await {
                            Ok(p) => {
                                self.probed_link.store(j, Ordering::Relaxed);
                                return Ok(p);
                            }
                            Err(e2) if e2.fatal => {
                                self.mark_dead(j, &e2);
                            }
                            Err(_) => {}
                        }
                    }
                    return Err(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| DlError::fatal("Every link for this file has failed")))
    }

    /// A single probe is never enough reason to throw away real progress: a rate limiter's error page, an
    /// edge-cache miss or a load balancer hiccup looks exactly like "the file changed". Only a consistent
    /// mismatch is trusted.
    async fn confirm_probe(&self, mut probe: net::Probe) -> DlResult<net::Probe> {
        let has_progress = !self.segs.lock().is_empty() && std::path::Path::new(&self.part_path()).exists();
        let known = self.total.load(Ordering::Relaxed);
        if !has_progress || (probe.resumable && probe.total == known) {
            return Ok(probe);
        }
        for i in 0..2u64 {
            c(&self.run, tokio::time::sleep(Duration::from_secs(3 + i * 3))).await?;
            match c(&self.run, self.probe_any()).await? {
                Ok(again) => {
                    if again.resumable && again.total == known {
                        return Ok(again);
                    }
                    probe = again;
                }
                Err(e) if e.retryable() => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(probe)
    }

    /// Reconciles saved progress with what the server says now.
    fn apply_probe(&self, p: &net::Probe) {
        let Some(it) = self.mgr.item(&self.id) else { return };
        let mut has_progress = !self.segs.lock().is_empty() && std::path::Path::new(&it.part_path()).exists();
        if has_progress && (!p.resumable || p.total != it.total_size || net::validator_changed(self.validator_for(&p.url).as_deref(), p.validator.as_deref())) {
            // The file on the server changed (or lost range support): the bytes we have are useless.
            self.segs.lock().clear();
            let _ = std::fs::remove_file(it.part_path());
            has_progress = false;
        }
        let new_name = (it.auto_name && !has_progress)
            .then(|| p.file_name.clone())
            .flatten()
            .map(|n| self.mgr.unique_name(&it.save_folder, &n, Some(&it.id), &[]));
        self.mgr.with_item(&self.id, |it| {
            if let Some(n) = new_name {
                it.file_name = n;
            }
            it.auto_name = false;
            it.total_size = p.total;
            it.resumable = p.resumable;
            // Items saved before validators were recorded retain their previous resume behaviour.
            if !has_progress {
                it.http_validator = p.validator.clone();
                it.http_validator_url = p.validator.as_ref().map(|_| p.url.clone());
            }
            if it.expected_hash.is_empty() {
                if let Some(sha) = &p.sha256 {
                    it.expected_hash = sha.clone();
                }
            }
            if !has_progress {
                it.downloaded = 0;
            }
        });
        self.total.store(p.total, Ordering::Relaxed);
        if !has_progress {
            self.run.current.store(0, Ordering::Relaxed);
        }
    }

    fn check_disk_space(&self, folder: &str) -> DlResult<()> {
        let total = self.total.load(Ordering::Relaxed);
        if total <= 0 {
            return Ok(());
        }
        let Some(free) = crate::win::free_space(folder) else { return Ok(()) };
        let need = total - self.run.current.load(Ordering::Relaxed);
        if free < need + 64 * 1024 * 1024 {
            let drive: String = folder.chars().take(2).collect();
            return Err(DlError::fatal(format!("Not enough space on {drive} — needs {}, {} free", fmt::size(need), fmt::size(free))));
        }
        Ok(())
    }

    fn target(&self) -> i32 {
        (self.connections.load(Ordering::Relaxed) - self.penalty.load(Ordering::Relaxed)).max(1)
    }

    /// Lets exactly one worker retire when there are more workers than the current target.
    fn surplus(&self) -> bool {
        let wc = self.worker_count.load(Ordering::Relaxed);
        wc > self.target() && self.worker_count.compare_exchange(wc, wc - 1, Ordering::SeqCst, Ordering::Relaxed).is_ok()
    }

    fn set_fatal(&self, e: DlError) {
        let mut f = self.fatal.lock();
        if f.is_none() {
            *f = Some(e);
        }
    }

    // ------------------------------------------------------------------ multi-connection

    async fn segmented(self: Arc<Self>) -> DlResult<()> {
        let part = self.part_path();
        let total = self.total.load(Ordering::Relaxed);
        let fresh = self.segs.lock().is_empty() || !std::path::Path::new(&part).exists();
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(fresh).open(&part)?;
        if fresh {
            crate::win::set_sparse(&file);
            let n = (total / MIN_SEGMENT).clamp(1, self.connections.load(Ordering::Relaxed).max(1) as i64);
            let size = total / n;
            let list: Vec<Seg> = (0..n)
                .map(|i| {
                    let end = if i == n - 1 { total - 1 } else { (i + 1) * size - 1 };
                    Arc::new(Mutex::new(Segment { start: i * size, end, downloaded: 0, active: false }))
                })
                .collect();
            *self.segs.lock() = list;
            self.run.current.store(0, Ordering::Relaxed);
        }
        if file.metadata()?.len() != total as u64 {
            file.set_len(total as u64)?;
        }
        let file = Arc::new(file);
        // Take the reference sample for mirror checks now, while the probed link is known to work.
        if self.url_count() > 1 {
            if let Ok(s) = self.sample(self.probed_link.load(Ordering::Relaxed)).await {
                *self.reference.lock().await = Some(s);
            }
        }

        let mut workers = JoinSet::new();
        let result = loop {
            if self.run.is_cancelled() {
                break Err(DlError::cancelled());
            }
            if let Some(e) = self.fatal.lock().clone() {
                break Err(e);
            }
            while workers.try_join_next().is_some() {}
            if self.segs.lock().iter().all(|s| s.lock().done()) {
                break Ok(());
            }
            // Connection count is re-read every pass, so it can be changed while downloading.
            if let Some(n) = self.mgr.with_item(&self.id, |it| it.connections) {
                self.connections.store(n, Ordering::Relaxed);
            }
            while (workers.len() as i32) < self.target() {
                let Some(seg) = self.acquire() else { break };
                let me = self.clone();
                let f = file.clone();
                workers.spawn(async move { me.worker(seg, f).await });
            }
            tokio::select! {
                _ = workers.join_next(), if !workers.is_empty() => {}
                _ = tokio::time::sleep(Duration::from_millis(400)) => {}
                _ = self.run.cancel.cancelled() => {}
            }
        };
        // Workers watch the same cancellation token; on success they have all finished already.
        if result.is_err() && !self.run.is_cancelled() {
            workers.abort_all();
        }
        while workers.join_next().await.is_some() {}
        self.worker_count.store(0, Ordering::Relaxed);
        for s in self.segs.lock().iter() {
            s.lock().active = false;
        }
        result
    }

    /// Hands out an idle unfinished segment, or splits the busiest one in half (work stealing).
    fn acquire(&self) -> Option<Seg> {
        let mut segs = self.segs.lock();
        let mut pick = segs.iter().find(|s| {
            let s = s.lock();
            !s.active && !s.done()
        }).cloned();
        if pick.is_none() {
            let mut victim: Option<Seg> = None;
            let mut best = 0;
            for s in segs.iter() {
                let g = s.lock();
                if g.active && g.remaining() > best {
                    best = g.remaining();
                    victim = Some(s.clone());
                }
            }
            if let Some(v) = victim.filter(|_| best >= 2 * MIN_SEGMENT) {
                let mut g = v.lock();
                let rem = g.remaining();
                if rem >= 2 * MIN_SEGMENT {
                    let mid = g.position() + rem / 2;
                    let new = Arc::new(Mutex::new(Segment { start: mid, end: g.end, downloaded: 0, active: false }));
                    g.end = mid - 1;
                    drop(g);
                    segs.push(new.clone());
                    segs.sort_by_key(|s| s.lock().start);
                    pick = Some(new);
                }
            }
        }
        if let Some(p) = &pick {
            p.lock().active = true;
        }
        pick
    }

    async fn worker(self: Arc<Self>, seg: Seg, file: Arc<std::fs::File>) {
        self.worker_count.fetch_add(1, Ordering::SeqCst);
        let mut counted = true;
        let mut attempt = 0;
        let mut had_error = false;
        let queue = self.queue();
        while !seg.lock().done() && !self.run.is_cancelled() {
            let Some(link) = self.pick_url() else {
                self.set_fatal(DlError::fatal("Every link for this file has failed"));
                break;
            };
            match self.fetch(link, &seg, &file, &queue, &mut attempt, &mut had_error).await {
                Ok(Flow::Continue) => {}
                Ok(Flow::Retire) => {
                    counted = false;
                    break;
                }
                Err(e) if e.kind == ErrKind::Cancelled || self.run.is_cancelled() => break,
                Err(e) if e.kind == ErrKind::Changed => {
                    self.set_fatal(e);
                    break;
                }
                Err(e) if local_fault(&e) => {
                    // Disk full, access denied…: no link can help, and none of them is to blame.
                    self.set_fatal(e);
                    break;
                }
                Err(e) => {
                    if e.fatal {
                        // This link is unusable; carry on with the others if any remain.
                        if self.mark_dead(link, &e) {
                            continue;
                        }
                        self.set_fatal(e);
                        break;
                    }
                    if e.status == 429 || e.status == 503 {
                        // The server wants fewer connections: back off for this run.
                        if self.connections.load(Ordering::Relaxed) - self.penalty.load(Ordering::Relaxed) > 1 {
                            let p = self.penalty.fetch_add(1, Ordering::SeqCst) + 1;
                            self.run.throttled.store(p, Ordering::Relaxed);
                        }
                        if self.surplus() {
                            counted = false;
                            break;
                        }
                    }
                    attempt += 1;
                    if !e.retryable() || attempt > self.max_retries {
                        self.set_fatal(e);
                        break;
                    }
                    had_error = true;
                    self.item_msg(format!("retry {attempt}: {}", e.msg));
                    // With backup links, the next attempt automatically goes to another link.
                    let wait = if self.url_count() > 1 { Duration::from_secs(1) } else { backoff(attempt) };
                    if c(&self.run, tokio::time::sleep(wait)).await.is_err() {
                        break;
                    }
                }
            }
        }
        if counted {
            self.worker_count.fetch_sub(1, Ordering::SeqCst);
        }
        seg.lock().active = false;
    }

    fn validator_for(&self, url: &str) -> Option<String> {
        self.mgr.with_item(&self.id, |it| {
            it.http_validator.as_ref().filter(|_| it.http_validator_url.as_deref() == Some(url)).cloned()
        }).flatten()
    }

    async fn fetch(&self, link: usize, seg: &Seg, file: &std::fs::File, queue: &str, attempt: &mut i32, had_error: &mut bool) -> DlResult<Flow> {
        if !self.ensure_same_content(link).await? {
            return Ok(Flow::Continue); // link dropped — pick another
        }
        let (from, to) = {
            let s = seg.lock();
            (s.position(), s.end)
        };
        let total = self.total.load(Ordering::Relaxed);
        let url = self.url_at(link);
        let req = net::build(&url, &self.headers, &self.ua, Some((from, Some(to))), Method::GET);
        let validator = self.validator_for(&url);
        let req = net::if_range(req, validator.as_deref());
        let mut resp = match c(&self.run, tokio::time::timeout(self.stall, req.send())).await? {
            Ok(r) => r?,
            Err(_) => return Err(DlError::timeout(format!("No data for {}s", self.stall.as_secs()))),
        };
        net::ensure_ok(&resp)?;
        if net::range_changed(validator.as_deref(), resp.status(), resp.headers()) {
            return Err(DlError { msg: "The file on the server changed".into(), fatal: true, status: 0, kind: ErrKind::Changed });
        }
        if resp.status() != StatusCode::PARTIAL_CONTENT {
            return Err(DlError::fatal("The server doesn't support resuming (byte ranges)"));
        }
        let cr = net::content_range(&resp);
        if let Some((_, _, Some(len))) = cr {
            if len != total {
                return Err(DlError::fatal(format!("This link serves a different file ({} instead of {})", fmt::size(len), fmt::size(total))));
            }
        }
        if cr.map(|c| c.0) != Some(from) {
            return Err(DlError::transient("The server sent the wrong byte range"));
        }

        struct Live<'a>(&'a AtomicI32);
        impl Drop for Live<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }
        self.run.active.fetch_add(1, Ordering::Relaxed);
        let _live = Live(&self.run.active);

        loop {
            let chunk = match c(&self.run, tokio::time::timeout(self.stall, resp.chunk())).await? {
                Ok(r) => r?,
                Err(_) => return Err(DlError::timeout(format!("No data for {}s", self.stall.as_secs()))),
            };
            let Some(data) = chunk else { break };
            if data.is_empty() {
                continue;
            }
            *attempt = 0;
            if *had_error {
                *had_error = false;
                self.item_msg("");
            }
            self.mgr.throttle(&self.run, queue, data.len()).await;
            let done = {
                let mut s = seg.lock();
                // `end` may have shrunk because another connection stole the tail.
                let pos = s.position();
                let take = (data.len() as i64).min(s.end - pos + 1);
                if take > 0 {
                    let mut buf = &data[..take as usize];
                    let mut at = pos as u64;
                    while !buf.is_empty() {
                        let n = file.seek_write(buf, at)?;
                        if n == 0 {
                            return Err(DlError::from(std::io::Error::other("write failed")));
                        }
                        buf = &buf[n..];
                        at += n as u64;
                    }
                    s.downloaded += take;
                    self.run.bytes.fetch_add(take, Ordering::Relaxed);
                    self.run.current.fetch_add(take, Ordering::Relaxed);
                }
                s.done()
            };
            if done {
                return Ok(Flow::Continue);
            }
            if self.surplus() {
                return Ok(Flow::Retire);
            }
        }
        if !seg.lock().done() {
            return Err(DlError { msg: "The server closed the connection early".into(), fatal: false, status: 0, kind: ErrKind::Io });
        }
        Ok(Flow::Continue)
    }

    // ------------------------------------------------------------------ single connection

    /// For servers without range support (or unknown size): one stream, restarted on failure.
    async fn single(&self, total: i64) -> DlResult<()> {
        self.segs.lock().clear();
        let part = self.part_path();
        let queue = self.queue();
        let mut attempt = 1;
        loop {
            self.run.current.store(0, Ordering::Relaxed);
            let Some(link) = self.pick_url() else { return Err(DlError::fatal("Every link for this file has failed")) };
            let r: DlResult<()> = async {
                let req = net::build(&self.url_at(link), &self.headers, &self.ua, None, Method::GET);
                let mut resp = match c(&self.run, tokio::time::timeout(self.stall, req.send())).await? {
                    Ok(r) => r?,
                    Err(_) => return Err(DlError::timeout(format!("No data for {}s", self.stall.as_secs()))),
                };
                net::ensure_ok(&resp)?;
                use std::io::Write;
                let mut f = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&part)?);
                self.run.active.store(1, Ordering::Relaxed);
                loop {
                    let chunk = match c(&self.run, tokio::time::timeout(self.stall, resp.chunk())).await? {
                        Ok(r) => r?,
                        Err(_) => return Err(DlError::timeout(format!("No data for {}s", self.stall.as_secs()))),
                    };
                    let Some(data) = chunk else { break };
                    f.write_all(&data)?;
                    self.run.current.fetch_add(data.len() as i64, Ordering::Relaxed);
                    self.run.bytes.fetch_add(data.len() as i64, Ordering::Relaxed);
                    self.mgr.throttle(&self.run, &queue, data.len()).await;
                }
                f.flush()?;
                let got = self.run.current.load(Ordering::Relaxed);
                if total > 0 && got != total {
                    return Err(DlError { msg: "The server closed the connection early".into(), fatal: false, status: 0, kind: ErrKind::Io });
                }
                if total <= 0 {
                    self.total.store(got, Ordering::Relaxed);
                    self.mgr.with_item(&self.id, |it| it.total_size = got);
                }
                Ok(())
            }
            .await;
            self.run.active.store(0, Ordering::Relaxed);
            match r {
                Ok(()) => return Ok(()),
                Err(e) if e.kind == ErrKind::Cancelled || self.run.is_cancelled() => return Err(DlError::cancelled()),
                Err(e) if local_fault(&e) => return Err(e),
                Err(e) => {
                    if e.fatal && self.url_count() > 1 {
                        if !self.mark_dead(link, &e) {
                            return Err(e);
                        }
                        continue;
                    }
                    if !e.retryable() || attempt > self.max_retries {
                        return Err(e);
                    }
                    self.item_msg(format!("retry {attempt} from zero (server can't resume): {}", e.msg));
                    c(&self.run, tokio::time::sleep(backoff(attempt))).await?;
                    attempt += 1;
                }
            }
        }
    }

    // ------------------------------------------------------------------ completion

    async fn finish(&self) -> DlResult<()> {
        if self.run.is_cancelled() { return Err(DlError::cancelled()); }
        let done = self.run.current.load(Ordering::Relaxed);
        let Some(it) = self.mgr.item(&self.id) else { return Ok(()) };
        let final_name = self.mgr.unique_name(&it.save_folder, &it.file_name, Some(&it.id), &[]);
        let final_path = std::path::Path::new(&it.save_folder).join(&final_name).to_string_lossy().into_owned();
        let expected = it.expected_hash.trim().to_lowercase();
        if let Some(algo) = crate::util::algo_for(&expected) {
            self.mgr.with_item(&self.id, |it| {
                it.verification_pending = true;
                it.verify_progress = 0.0;
                it.set_status(DlStatus::Verifying);
            });
            let cancel = Arc::new(AtomicBool::new(false));
            let (mgr, id, path, c2) = (self.mgr.clone(), self.id.clone(), it.part_path(), cancel.clone());
            let mut job = tauri::async_runtime::spawn_blocking(move || {
                crate::util::hash_file(&path, algo, |p| {
                    mgr.with_item(&id, |it| it.verify_progress = p);
                }, &c2)
            });
            let actual = tokio::select! {
                r = &mut job => r.map_err(|e| DlError::fatal(e.to_string()))?,
                _ = self.run.cancel.cancelled() => {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = job.await; // Let the reader release the file before a resume or removal.
                    return Err(DlError::cancelled());
                }
            };
            let actual = actual?;
            if actual != expected {
                return Err(DlError::fatal(format!("Checksum mismatch — file is corrupt (got {}…, expected {}…)", &actual[..12], &expected[..12])));
            }
        }
        if self.run.is_cancelled() { return Err(DlError::cancelled()); }
        self.mgr.with_item(&self.id, |it| {
            if self.run.is_cancelled() { return Err(DlError::cancelled()); }
            std::fs::rename(it.part_path(), &final_path)?;
            it.file_name = final_name;
            if it.total_size <= 0 { it.total_size = done; }
            it.downloaded = it.total_size;
            it.segments.lock().clear();
            it.error_message.clear();
            it.verification_pending = false;
            it.set_status(DlStatus::Completed);
            it.completed_at = Some(chrono::Local::now());
            Ok(())
        }).unwrap_or_else(|| Err(DlError::cancelled()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Arc<Manager>, Item, std::path::PathBuf) {
        let root = std::env::current_dir().unwrap().join("target").join(format!("http-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let it = Item { save_folder: root.to_string_lossy().into_owned(), file_name: "file.bin".into(), total_size: 1024 * 1024, ..Default::default() };
        std::fs::write(it.part_path(), vec![7u8; it.total_size as usize]).unwrap();
        it.segments.lock().push(Arc::new(Mutex::new(Segment { start: 0, end: it.total_size - 1, downloaded: it.total_size, active: false })));
        let mgr = Manager::new();
        mgr.st.lock().items.push(it.clone());
        (mgr, it, root)
    }

    #[tokio::test]
    async fn pause_during_verification_keeps_partial_file_for_recheck() {
        let (mgr, mut it, root) = fixture();
        it.expected_hash = crate::util::hash_file(&it.part_path(), crate::util::Algo::Sha256, |_| {}, &AtomicBool::new(false)).unwrap();
        mgr.st.lock().items[0] = it.clone();
        let run = Run::test_run(&it.id);
        run.current.store(it.total_size, Ordering::Relaxed);
        let ctx = Ctx::new(mgr.clone(), run.clone(), &it);
        let cancel = run.cancel.clone();
        let watcher = mgr.clone();
        let id = it.id.clone();
        let task = tokio::spawn(async move {
            while watcher.item(&id).unwrap().status != DlStatus::Verifying { tokio::task::yield_now().await; }
            cancel.cancel();
        });
        assert_eq!(ctx.finish().await.unwrap_err().kind, ErrKind::Cancelled);
        task.await.unwrap();
        assert_ne!(mgr.item(&it.id).unwrap().status, DlStatus::Completed);
        assert!(std::path::Path::new(&it.part_path()).is_file());
        assert!(!std::path::Path::new(&it.full_path()).exists());
        assert!(!it.segments.lock().is_empty());
        // A non-resumable download also rechecks locally, including after persistence.
        mgr.with_item(&it.id, |it| { it.status = DlStatus::Paused; it.downloaded = it.total_size; it.segments.lock().clear(); });
        let saved: Item = serde_json::from_str(&serde_json::to_string(&mgr.item(&it.id).unwrap()).unwrap()).unwrap();
        assert!(saved.verification_pending);
        mgr.st.lock().items[0] = saved;
        let resumed = Run::test_run(&it.id);
        super::run(mgr.clone(), resumed).await;
        assert_eq!(mgr.item(&it.id).unwrap().status, DlStatus::Completed);
        assert!(std::path::Path::new(&it.full_path()).is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_validator_discards_same_size_partial_data() {
        for (saved, source, current, changed) in [
            (None, None, "\"new\"", false),
            (Some("\"old\""), None, "\"new\"", false),
            (Some("\"old\""), Some("https://example.invalid/file"), "\"new\"", true),
            (Some("\"old\""), Some("https://example.invalid/file"), "Wed, 21 Oct 2015 07:28:00 GMT", false),
            (Some("Wed, 21 Oct 2015 07:28:00 GMT"), Some("https://example.invalid/file"), "\"new\"", false),
        ] {
            let (mgr, it, root) = fixture();
            mgr.with_item(&it.id, |it| {
                it.http_validator = saved.map(str::to_string);
                it.http_validator_url = source.map(str::to_string);
            });
            let ctx = Ctx::new(mgr.clone(), Run::test_run(&it.id), &it);
            ctx.apply_probe(&net::Probe { url: "https://example.invalid/file".into(), total: it.total_size, resumable: true, file_name: None, sha256: None, validator: Some(current.into()) });
            assert_eq!(it.segments.lock().is_empty(), changed);
            assert_eq!(std::path::Path::new(&it.part_path()).exists(), !changed);
            assert_eq!(mgr.item(&it.id).unwrap().http_validator.as_deref(), if changed { Some(current) } else { saved });
            assert_eq!(mgr.item(&it.id).unwrap().http_validator_url.as_deref(), source);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn mirror_with_different_etag_preserves_progress_and_omits_if_range() {
        use axum::{http::HeaderMap, routing::get, Router};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mirror = format!("http://{}/file", listener.local_addr().unwrap());
        let app = Router::new().route("/file", get(|headers: HeaderMap| async move {
            assert!(!headers.contains_key("if-range"));
            assert_eq!(headers["range"], "bytes=10-13");
            (StatusCode::PARTIAL_CONTENT, [("etag", "\"mirror\""), ("content-range", "bytes 10-13/1048576")], vec![7u8; 4])
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let (mgr, mut it, root) = fixture();
        it.url = "https://example.invalid/file".into();
        it.mirrors = vec![mirror.clone()];
        it.http_validator = Some("\"primary\"".into());
        it.http_validator_url = Some(it.url.clone());
        let saved: Item = serde_json::from_str(&serde_json::to_string(&it).unwrap()).unwrap();
        assert_eq!(saved.http_validator_url, it.http_validator_url);
        mgr.st.lock().items[0] = it.clone();
        let ctx = Ctx::new(mgr.clone(), Run::test_run(&it.id), &it);
        ctx.sync_urls();
        ctx.apply_probe(&net::Probe { url: mirror, total: it.total_size, resumable: true, file_name: None, sha256: None, validator: Some("\"mirror\"".into()) });
        assert!(!it.segments.lock().is_empty());
        assert!(std::path::Path::new(&it.part_path()).exists());
        assert_eq!(ctx.validator_for(&it.url).as_deref(), Some("\"primary\""));
        // Content sampling is independent of validator scope; this link has already passed it.
        ctx.links.lock().verified[1] = true;
        let seg = Arc::new(Mutex::new(Segment { start: 10, end: 13, downloaded: 0, active: true }));
        let file = std::fs::OpenOptions::new().write(true).open(it.part_path()).unwrap();
        let result = ctx.fetch(1, &seg, &file, "", &mut 0, &mut false).await;
        server.abort();
        assert!(result.is_ok());
        assert_eq!(seg.lock().downloaded, 4);
        drop(file);
        std::fs::remove_dir_all(root).unwrap();
    }
}
