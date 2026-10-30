//! Discord notifications through a channel webhook. Each event has its own switch in Settings; messages are
//! sent in the background, one at a time, and a failure only reaches the log - it never slows a download.

use crate::manager::Manager;
use crate::model::*;
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

/// Only real Discord webhook links are accepted (and the link is sent nowhere else).
pub fn valid_webhook(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url.trim()) else { return false };
    u.scheme() == "https"
        && matches!(u.host_str(), Some("discord.com" | "discordapp.com" | "canary.discord.com" | "ptb.discord.com"))
        && u.path().starts_with("/api/webhooks/")
        && u.path().trim_end_matches('/').split('/').count() >= 5
}

/// Messages go out in order through one background sender, so a burst can't trip Discord's rate limit.
static QUEUE: LazyLock<tokio::sync::mpsc::UnboundedSender<(String, String)>> = LazyLock::new(|| {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    tauri::async_runtime::spawn(async move {
        while let Some((url, text)) = rx.recv().await {
            if let Err(e) = post(&url, &text).await {
                eprintln!("Discord notification not sent: {e}");
            }
        }
    });
    tx
});

/// Sends one message: retries a few times on network errors and waits out Discord's 429 Retry-After.
pub async fn post(url: &str, text: &str) -> Result<(), String> {
    if !valid_webhook(url) {
        return Err("That isn't a Discord webhook link (https://discord.com/api/webhooks/…)".into());
    }
    let body = json!({ "username": "FluxDM", "content": clip(text, 1900), "allowed_mentions": { "parse": [] } });
    let mut last = String::new();
    for attempt in 0..4u32 {
        let r = crate::net::HTTP.post(url).json(&body).timeout(Duration::from_secs(20)).send().await;
        match r {
            Ok(resp) => {
                let code = resp.status().as_u16();
                if resp.status().is_success() {
                    return Ok(());
                }
                if code == 429 {
                    let wait = resp.json::<serde_json::Value>().await.ok().and_then(|v| v["retry_after"].as_f64()).unwrap_or(2.0);
                    tokio::time::sleep(Duration::from_secs_f64(wait.clamp(0.5, 60.0))).await;
                    last = "Discord asked to slow down".into();
                    continue;
                }
                return Err(match code {
                    401 | 403 | 404 => "Discord says this webhook doesn't exist any more (deleted, or the link is wrong). Make a new one in the channel's Integrations settings.".into(),
                    _ => format!("Discord answered {code}"),
                });
            }
            Err(e) => {
                last = if e.is_connect() || e.is_timeout() { "Couldn't reach Discord (no internet?)".into() } else { e.to_string() };
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt + 1))).await;
            }
        }
    }
    Err(last)
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s.to_string() }
}

fn send(s: &Settings, text: String) {
    if s.discord_webhook.trim().is_empty() {
        return;
    }
    let _ = QUEUE.send((s.discord_webhook.trim().to_string(), text));
}

fn took(it: &Item) -> String {
    let secs = it.active_seconds.max(0.0) as i64;
    if secs <= 0 { return String::new(); }
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!(" · {h} h {m} min") } else if m > 0 { format!(" · {m} min") } else { format!(" · {s} s") }
}

pub fn finished(s: &Settings, it: &Item) {
    if s.discord_on_finished {
        send(s, finished_text(it));
    }
}

pub fn finished_text(it: &Item) -> String {
    format!("✅ **{}** finished · {}{}", it.file_name, fmt::size(it.total_size.max(it.downloaded)), took(it))
}

pub fn failed(s: &Settings, it: &Item) {
    if s.discord_on_failed {
        send(s, failed_text(it));
    }
}

pub fn failed_text(it: &Item) -> String {
    let why = if it.error_message.trim().is_empty() { "unknown error" } else { it.error_message.trim() };
    format!("❌ **{}** failed: {}", it.file_name, clip(why, 300))
}

pub fn queue_finished(s: &Settings, name: &str, done: usize, failures: usize) {
    if s.discord_on_queue {
        send(s, queue_text(name, done, failures));
    }
}

pub fn queue_text(name: &str, done: usize, failures: usize) -> String {
    if failures > 0 {
        format!("⚠️ Queue **{name}** stopped · {done} done, {failures} failed")
    } else {
        format!("📋 Queue **{name}** finished · {done} download{}", if done == 1 { "" } else { "s" })
    }
}

/// When each drive was last reported low (at most one message per drive per hour).
static DISK_SENT: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Drives (like "D:") that are below the threshold and weren't reported in the last hour.
pub fn low_drives(drives: &[(String, i64)], threshold: i64, sent: &mut HashMap<String, Instant>, now: Instant) -> Vec<(String, i64)> {
    let mut out = vec![];
    for (drive, free) in drives {
        if *free >= threshold { continue; }
        if sent.get(drive).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(3600)) { continue; }
        sent.insert(drive.clone(), now);
        out.push((drive.clone(), *free));
    }
    out
}

/// "C:\Users\x\Downloads" -> "C:"; "\\server\share\x" -> "\\server\share".
fn drive_of(folder: &str) -> String {
    let p = folder.replace('/', "\\");
    if let Some(rest) = p.strip_prefix("\\\\") {
        let parts: Vec<&str> = rest.split('\\').take(2).collect();
        return format!("\\\\{}", parts.join("\\"));
    }
    p.chars().take(2).collect::<String>().to_uppercase()
}

/// Called every ~30 s: disk-space warnings and the daily summary.
pub fn tick(mgr: &Arc<Manager>) {
    let s = mgr.settings();
    if s.discord_webhook.trim().is_empty() {
        return;
    }
    if s.discord_on_disk && s.discord_disk_gb > 0 {
        let folders: Vec<String> = {
            let items: Vec<(String, String)> = mgr.st.lock().items.iter().map(|i| (i.id.clone(), i.save_folder.clone())).collect();
            items.into_iter().filter(|(id, _)| mgr.is_running(id)).map(|(_, f)| f).collect()
        };
        let mut drives: Vec<(String, i64)> = vec![];
        for f in folders {
            let d = drive_of(&f);
            if drives.iter().any(|x| x.0 == d) { continue; }
            if let Some(free) = crate::win::free_space(&f) { drives.push((d, free)); }
        }
        let low = low_drives(&drives, s.discord_disk_gb as i64 * 1024 * 1024 * 1024, &mut DISK_SENT.lock(), Instant::now());
        for (d, free) in low {
            send(&s, format!("💾 Disk **{d}** is almost full · {} free (warning level {} GB)", fmt::size(free), s.discord_disk_gb));
        }
    }
    if s.discord_daily {
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let due = crate::model::parse_hhmm(&s.discord_daily_time).is_some_and(|t| (now.hour_min()) >= t);
        if due && s.discord_daily_sent != today {
            let text = {
                let mut st = mgr.st.lock();
                st.settings.discord_daily_sent = today.clone();
                daily_text(&st.items, st.stats.days.get(&today), now.date_naive())
            };
            mgr.mark_dirty();
            if let Some(text) = text {
                send(&s, text);
            }
        }
    }
}

trait HourMin {
    fn hour_min(&self) -> i32;
}
impl HourMin for chrono::DateTime<chrono::Local> {
    fn hour_min(&self) -> i32 {
        use chrono::Timelike;
        (self.hour() * 60 + self.minute()) as i32
    }
}

/// The day's summary, or None when nothing happened (then no message is sent).
pub fn daily_text(items: &[Item], day: Option<&DayStat>, date: chrono::NaiveDate) -> Option<String> {
    let finished = items.iter().filter(|i| i.status == DlStatus::Completed && i.completed_at.is_some_and(|t| t.date_naive() == date)).count();
    let failed = items.iter().filter(|i| i.status == DlStatus::Error && i.last_try.is_some_and(|t| t.date_naive() == date)).count();
    let bytes = day.map(|d| d.bytes).unwrap_or(0);
    if finished == 0 && failed == 0 && bytes <= 0 {
        return None;
    }
    let mut t = format!("📊 Today · {} downloaded · {finished} finished", fmt::size(bytes));
    if failed > 0 {
        t.push_str(&format!(" · {failed} failed"));
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_discord_webhooks_are_accepted() {
        assert!(valid_webhook("https://discord.com/api/webhooks/123/abcDEF"));
        assert!(valid_webhook("https://discordapp.com/api/webhooks/123/abc"));
        assert!(!valid_webhook("http://discord.com/api/webhooks/123/abc"));
        assert!(!valid_webhook("https://discord.com.evil.net/api/webhooks/123/abc"));
        assert!(!valid_webhook("https://discord.com/channels/1/2"));
        assert!(!valid_webhook("https://discord.com/api/webhooks/"));
        assert!(!valid_webhook(""));
    }

    #[test]
    fn messages_read_well() {
        let it = Item { file_name: "Movie.mkv".into(), total_size: 5 * 1024 * 1024 * 1024, active_seconds: 18.0 * 60.0, ..Default::default() };
        let t = finished_text(&it);
        assert!(t.contains("Movie.mkv") && t.contains("5") && t.contains("18 min"), "{t}");
        let mut bad = it.clone();
        bad.error_message = "the link expired".into();
        assert!(failed_text(&bad).contains("the link expired"));
        assert!(queue_text("Night", 5, 0).contains("5 downloads"));
        assert!(queue_text("Night", 3, 2).contains("2 failed"));
    }

    #[test]
    fn disk_warnings_are_sent_once_an_hour_per_drive() {
        let mut sent = HashMap::new();
        let t0 = Instant::now();
        let drives = vec![("C:".to_string(), 5), ("D:".to_string(), 500)];
        assert_eq!(low_drives(&drives, 100, &mut sent, t0).len(), 1);
        assert!(low_drives(&drives, 100, &mut sent, t0 + Duration::from_secs(600)).is_empty());
        assert_eq!(low_drives(&drives, 100, &mut sent, t0 + Duration::from_secs(3700)).len(), 1);
        assert_eq!(drive_of("d:\\Games\\x"), "D:");
        assert_eq!(drive_of("\\\\nas\\share\\x"), "\\\\nas\\share");
    }

    #[test]
    fn daily_summary_is_skipped_when_nothing_happened() {
        let date = chrono::Local::now().date_naive();
        assert!(daily_text(&[], None, date).is_none());
        let done = Item { status: DlStatus::Completed, completed_at: Some(chrono::Local::now()), ..Default::default() };
        let t = daily_text(&[done], Some(&DayStat { bytes: 1024, ..Default::default() }), date).unwrap();
        assert!(t.contains("1 finished"), "{t}");
    }
}
