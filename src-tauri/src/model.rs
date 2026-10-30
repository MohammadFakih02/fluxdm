//! The download list, queues and settings, stored in the same JSON files (and the same PascalCase
//! shape) the .NET FluxDM wrote, so an existing download list, queues and stats carry straight over.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const MAX_CONNECTIONS: i32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DlStatus {
    #[default]
    Paused,
    Queued,
    Connecting,
    Downloading,
    Verifying,
    Completed,
    Error,
}

impl DlStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::Paused => "Paused",
            Self::Queued => "Queued",
            Self::Connecting => "Connecting",
            Self::Downloading => "Downloading",
            Self::Verifying => "Verifying",
            Self::Completed => "Completed",
            Self::Error => "Error",
        }
    }
    pub fn is_active(self) -> bool {
        matches!(self, Self::Connecting | Self::Downloading | Self::Verifying)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PostAction {
    #[default]
    None,
    Exit,
    Sleep,
    Hibernate,
    Shutdown,
    Restart,
}

// ------------------------------------------------------------------ dates
//
// .NET wrote local times either with an offset ("2026-09-27T14:03:12.1234567+03:00") or without one
// (DateTime.Today, "0001-01-01T00:00:00"). Both read back as local times.

pub type When = DateTime<Local>;

pub fn parse_when(s: &str) -> Option<When> {
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Local));
    }
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
        .ok()?;
    Local.from_local_datetime(&naive).earliest()
}

pub fn min_when() -> When {
    Local.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap()
}

pub mod when {
    use super::*;
    pub fn serialize<S: Serializer>(d: &When, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&d.to_rfc3339())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<When, D::Error> {
        let s = String::deserialize(d)?;
        Ok(parse_when(&s).unwrap_or_else(min_when))
    }
}

pub mod when_opt {
    use super::*;
    pub fn serialize<S: Serializer>(d: &Option<When>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_str(&d.to_rfc3339()),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<When>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        Ok(s.and_then(|s| parse_when(&s)))
    }
}

fn now() -> When {
    Local::now()
}

// ------------------------------------------------------------------ segments

/// A byte range [start, end] fetched by one connection. `downloaded` counts bytes already on disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Segment {
    pub start: i64,
    pub end: i64,
    pub downloaded: i64,
    #[serde(skip)]
    pub active: bool,
}

impl Segment {
    pub fn position(&self) -> i64 {
        self.start + self.downloaded
    }
    pub fn remaining(&self) -> i64 {
        (self.end - self.position() + 1).max(0)
    }
    pub fn done(&self) -> bool {
        self.position() > self.end
    }
}

pub type Seg = Arc<Mutex<Segment>>;

// ------------------------------------------------------------------ items

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Item {
    pub id: String,
    pub url: String,
    pub file_name: String,
    pub save_folder: String,
    /// Stable working location, retained across settings changes and restarts.
    pub incomplete_folder: String,
    pub existing_file_policy: String,
    pub total_size: i64,
    pub downloaded: i64,
    pub status: DlStatus,
    pub connections: i32,
    #[serde(rename = "SpeedLimitKBps")]
    pub speed_limit_kbps: i32,
    pub queue_name: String,
    pub queue_order: i64,
    pub error_message: String,
    #[serde(with = "when")]
    pub added_at: When,
    #[serde(with = "when_opt")]
    pub last_try: Option<When>,
    #[serde(with = "when_opt")]
    pub completed_at: Option<When>,
    pub resumable: bool,
    pub http_validator: Option<String>,
    pub http_validator_url: Option<String>,
    pub auto_name: bool,
    pub expected_hash: String,
    pub verification_pending: bool,
    pub headers: String,
    pub retry_count: i32,
    pub mirrors: Vec<String>,

    // torrents
    pub is_torrent: bool,
    pub torrent_file: String,
    pub info_hash: String,
    pub skipped_files: Vec<i32>,
    pub torrent_skip_patterns: String,
    pub torrent_mark_unfinished: bool,
    pub seed_seconds: f64,
    /// File index -> priority (1 Low, 3 High, 4 Maximum; Normal isn't stored). New in 2.0.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub file_priorities: std::collections::BTreeMap<i32, u8>,
    /// Torrent: fetch pieces in file order (for watching while it downloads). New in 2.0.3.
    pub sequential: bool,
    /// Open the file (or the torrent's folder) as soon as it finishes. New in 2.2.
    pub open_when_done: bool,
    #[serde(rename = "UploadLimitKBps")]
    pub upload_limit_kbps: i32,
    #[serde(rename = "SeedingLimitKBps")]
    pub seeding_limit_kbps: i32,
    #[serde(skip)]
    pub seeding: bool,
    pub extra_trackers: Vec<String>,
    pub total_uploaded: i64,
    pub active_seconds: f64,
    #[serde(with = "when_opt")]
    pub last_activity_at: Option<When>,
    pub tags: Vec<String>,

    // pictures
    pub icon_file: String,
    pub icon_source: String,
    #[serde(with = "when_opt")]
    pub icon_lookup_at: Option<When>,
    pub cover_file: String,
    pub banner_file: String,
    pub art_color: String,
    pub banner_revision: u8,
    pub steam_title: String,
    pub console_title: String,

    // video
    pub is_video: bool,
    pub video_format: String,
    pub video_playlist: bool,
    /// Chosen playlist entries for --playlist-items (e.g. "1,3,5-9"); empty = all.
    pub video_playlist_items: String,
    /// Only this part of the video, "start-end" (e.g. "1:30-2:45"); empty = whole video.
    pub video_section: String,
    /// The site's id for the video (from the probe), to spot duplicates.
    pub video_id: String,
    /// Private staging folder and completed files owned by this video job.
    pub video_work_folder: String,
    pub video_files: Vec<String>,

    /// Saved copy of the live segment list (the live one is shared with the download's connections).
    #[serde(rename = "Segments")]
    pub saved_segments: Vec<Segment>,

    // ---- transient ----
    #[serde(skip)]
    pub segments: Arc<Mutex<Vec<Seg>>>,
    #[serde(skip)]
    pub note: String,
    #[serde(skip)]
    pub speed: f64,
    #[serde(skip)]
    pub upload_speed: f64,
    #[serde(skip)]
    pub active_connections: i32,
    #[serde(skip)]
    pub verify_progress: f64,
    #[serde(skip)]
    pub not_before: Option<When>,
    #[serde(skip)]
    pub connected_seeds: i32,
    #[serde(skip)]
    pub connected_peers: i32,
    /// Swarm totals from the trackers (seeders / leechers), and distributed copies of what's still needed.
    #[serde(skip)]
    pub swarm_seeds: i32,
    #[serde(skip)]
    pub swarm_peers: i32,
    #[serde(skip)]
    pub availability: Option<f64>,
    #[serde(skip)]
    pub throttled: i32,
    #[serde(skip)]
    pub speed_history: Vec<f64>,
    #[serde(skip)]
    pub upload_history: Vec<f64>,
}

impl Default for Item {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            url: String::new(),
            file_name: String::new(),
            save_folder: String::new(),
            incomplete_folder: String::new(),
            existing_file_policy: String::new(),
            total_size: -1,
            downloaded: 0,
            status: DlStatus::Paused,
            connections: 8,
            speed_limit_kbps: 0,
            queue_name: String::new(),
            queue_order: now().timestamp_micros(),
            error_message: String::new(),
            added_at: now(),
            last_try: None,
            completed_at: None,
            resumable: false,
            http_validator: None,
            http_validator_url: None,
            auto_name: false,
            expected_hash: String::new(),
            verification_pending: false,
            headers: String::new(),
            retry_count: 0,
            mirrors: vec![],
            is_torrent: false,
            torrent_file: String::new(),
            info_hash: String::new(),
            skipped_files: vec![],
            torrent_skip_patterns: String::new(),
            torrent_mark_unfinished: false,
            seed_seconds: 0.0,
            file_priorities: Default::default(),
            sequential: false,
            open_when_done: false,
            upload_limit_kbps: 0,
            seeding_limit_kbps: 0,
            seeding: false,
            extra_trackers: vec![],
            total_uploaded: 0,
            active_seconds: 0.0,
            last_activity_at: None,
            tags: vec![],
            icon_file: String::new(),
            icon_source: String::new(),
            icon_lookup_at: None,
            cover_file: String::new(),
            banner_file: String::new(),
            art_color: String::new(),
            banner_revision: 0,
            steam_title: String::new(),
            console_title: String::new(),
            is_video: false,
            video_format: "best".into(),
            video_playlist: false,
            video_playlist_items: String::new(),
            video_section: String::new(),
            video_id: String::new(),
            video_work_folder: String::new(),
            video_files: vec![],
            saved_segments: vec![],
            segments: Arc::new(Mutex::new(vec![])),
            note: String::new(),
            speed: 0.0,
            upload_speed: 0.0,
            active_connections: 0,
            verify_progress: 0.0,
            not_before: None,
            connected_seeds: 0,
            swarm_seeds: 0,
            swarm_peers: 0,
            availability: None,
            connected_peers: 0,
            throttled: 0,
            speed_history: vec![],
            upload_history: vec![],
        }
    }
}

impl Item {
    pub fn storage_folder(&self) -> &str {
        if self.incomplete_folder.is_empty() { &self.save_folder } else { &self.incomplete_folder }
    }
    pub fn full_path(&self) -> String {
        std::path::Path::new(if self.is_torrent { self.storage_folder() } else { &self.save_folder }).join(&self.file_name).to_string_lossy().into_owned()
    }
    pub fn part_path(&self) -> String {
        if self.incomplete_folder.is_empty() { self.full_path() + ".fluxpart" }
        else { std::path::Path::new(&self.incomplete_folder).join(format!("{}.fluxpart", self.id)).to_string_lossy().into_owned() }
    }
    pub fn category(&self) -> &'static str {
        if self.is_torrent {
            "Torrents"
        } else if self.is_video && !self.video_format.starts_with("audio") {
            "Video"
        } else {
            category_of(&self.file_name)
        }
    }
    pub fn is_active(&self) -> bool {
        self.status.is_active()
    }
    pub fn set_status(&mut self, s: DlStatus) {
        self.status = s;
        if s != DlStatus::Downloading {
            self.speed = 0.0;
            self.active_connections = 0;
        }
    }
    pub fn set_connections(&mut self, n: i32) {
        self.connections = n.clamp(1, MAX_CONNECTIONS);
    }
    pub fn progress(&self) -> f64 {
        if self.status == DlStatus::Completed {
            100.0
        } else if self.total_size > 0 {
            (self.downloaded as f64 * 100.0 / self.total_size as f64).min(100.0)
        } else {
            0.0
        }
    }
    pub fn ratio(&self) -> f64 {
        if self.is_torrent && self.downloaded > 0 {
            self.total_uploaded as f64 / self.downloaded as f64
        } else {
            0.0
        }
    }
    pub fn status_text(&self) -> String {
        let e = &self.error_message;
        match self.status {
            DlStatus::Error => if e.is_empty() { "Error".into() } else { format!("Error: {e}") },
            DlStatus::Verifying if self.is_torrent => format!("Checking {:.0}%", self.verify_progress),
            DlStatus::Verifying => format!("Verifying {:.0}%", self.verify_progress),
            DlStatus::Paused if !e.is_empty() => format!("Paused: {e}"),
            DlStatus::Paused if self.is_torrent && !self.note.is_empty() => format!("Paused: {}", self.note),
            DlStatus::Connecting => if e.is_empty() { "Connecting…".into() } else { format!("Connecting… ({e})") },
            DlStatus::Downloading if !self.note.is_empty() => format!("Downloading · {}", self.note),
            DlStatus::Downloading => if e.is_empty() { "Downloading".into() } else { "Downloading · reconnecting".into() },
            DlStatus::Queued if self.not_before.is_some_and(|t| t > now()) => "Waiting to retry".into(),
            DlStatus::Completed if !self.note.is_empty() => format!("Complete · {}", self.note),
            DlStatus::Completed => "Complete".into(),
            s => s.name().into(),
        }
    }
    pub fn push_speed(&mut self, v: f64) {
        self.speed_history.push(v);
        self.upload_history.push(self.upload_speed);
        if self.upload_history.len() > 120 {
            self.upload_history.remove(0);
        }
        if self.speed_history.len() > 120 {
            self.speed_history.remove(0);
        }
    }
    /// Copies the live segments into the saved list (called before writing the list to disk).
    pub fn snapshot_segments(&mut self) {
        let segs = self.segments.lock();
        self.saved_segments = segs.iter().map(|s| {
            let s = s.lock();
            Segment { start: s.start, end: s.end, downloaded: s.downloaded, active: false }
        }).collect();
    }
}

// ------------------------------------------------------------------ queues

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct QueueDef {
    pub name: String,
    pub max_concurrent: i32,
    #[serde(rename = "SpeedLimitKBps")]
    pub speed_limit_kbps: i32,
    pub stop_when_empty: bool,
    pub retry_count: i32,
    pub retry_delay_sec: i32,
    pub start_on_launch: bool,
    pub start_at_enabled: bool,
    pub start_at: String,
    pub daily: bool,
    pub days_mask: i32,
    #[serde(with = "when")]
    pub once_date: When,
    pub stop_at_enabled: bool,
    pub stop_at: String,
    pub post_action: PostAction,
    pub force_shutdown: bool,
    #[serde(with = "when")]
    pub last_start_fire: When,
    #[serde(with = "when")]
    pub last_stop_fire: When,
    #[serde(skip)]
    pub running: bool,
    pub finish_notified: bool,
    pub schedule_slot: Option<chrono::NaiveDate>,
    pub last_start_slot: Option<chrono::NaiveDate>,
    pub last_stop_slot: Option<chrono::NaiveDate>,
    #[serde(skip)]
    pub started_ids: HashSet<String>,
}

impl Default for QueueDef {
    fn default() -> Self {
        Self {
            name: "Main queue".into(),
            max_concurrent: 2,
            speed_limit_kbps: 0,
            stop_when_empty: true,
            retry_count: 0,
            retry_delay_sec: 20,
            start_on_launch: false,
            start_at_enabled: false,
            start_at: "02:00".into(),
            daily: true,
            days_mask: 0x7F,
            once_date: now(),
            stop_at_enabled: false,
            stop_at: "08:00".into(),
            post_action: PostAction::None,
            force_shutdown: false,
            last_start_fire: min_when(),
            last_stop_fire: min_when(),
            running: false,
            finish_notified: false,
            schedule_slot: None,
            last_start_slot: None,
            last_stop_slot: None,
            started_ids: HashSet::new(),
        }
    }
}

// ------------------------------------------------------------------ settings

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct ColumnState {
    pub width: f64,
    pub order: i32,
    pub visible: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    // layout (the web UI keeps its own column layout here too)
    pub columns: HashMap<String, ColumnState>,
    pub columns_layout_version: i32,
    pub sidebar_width: f64,
    pub details_height: f64,

    pub show_icons: bool,
    pub lookup_covers_online: bool,
    pub background_color: String,
    pub accent_from_art: bool,
    pub accent_color: String,
    pub glass: String,
    pub line_glow: i32,
    pub art_behind_window: bool,
    pub art_selected_only: bool,
    /// The app icon theme: heat (the original), ice, neon, emerald or mono.
    pub icon_theme: String,
    /// How strongly the art behind the window shows, in percent of the measured default (25-500).
    pub art_strength: i32,
    /// A fixed blur for the art behind the window in px, or -1 for the automatic one.
    pub art_blur: i32,
    /// The same for the art on the selected download in the list: strength in percent (25-400), blur in px.
    pub card_art_strength: i32,
    pub card_art_blur: i32,
    /// The big banner above the details: strength in percent (25-150), blur in px.
    pub banner_strength: i32,
    pub banner_blur: i32,
    pub proxy_mode: String,
    pub proxy_host: String,
    pub proxy_port: u16,
    pub proxy_username: String,
    pub proxy_password: String,
    pub proxy_bypass_local: bool,
    pub existing_file_policy: String,
    pub incomplete_folder: String,
    pub default_folder: String,
    pub category_subfolders: bool,
    pub default_connections: i32,
    #[serde(rename = "GlobalSpeedLimitKBps")]
    pub global_speed_limit_kbps: i32,
    pub max_retries: i32,
    pub read_timeout_sec: i32,
    pub queue_retries: i32,
    pub max_manual_downloads: i32,
    pub last_add_queue: String,
    pub user_agent: String,
    pub clipboard_monitor: bool,
    pub clipboard_extensions: String,
    pub local_api_enabled: bool,
    pub local_api_port: i32,
    pub close_to_tray: bool,
    pub prevent_sleep: bool,
    pub notify_on_complete: bool,
    pub start_with_windows: bool,
    /// When FluxDM last asked to turn "Start with Windows" on (it asks again two weeks later while it is off).
    pub startup_asked_at: Option<chrono::DateTime<chrono::Local>>,
    pub theme: String,

    pub speed_schedule_enabled: bool,
    pub schedule_days_mask: u8,
    pub schedule_from: String,
    pub schedule_to: String,
    #[serde(rename = "ScheduleLimitKBps")]
    pub schedule_limit_kbps: i32,

    pub video_cookies_browser: String,
    pub default_video_format: String,
    /// "None" | "Download" | "Embed"
    pub video_subtitles: String,
    pub video_sub_langs: String,
    pub video_auto_subs: bool,
    /// Embed the thumbnail, metadata and chapters into the file.
    pub video_embed_meta: bool,
    /// "Off" | "Mark" | "Remove" (YouTube only)
    pub video_sponsorblock: String,
    /// A Netscape cookies.txt used instead of browser cookies when set.
    pub video_cookies_file: String,
    /// The raw yt-dlp -f selector for the "custom" format.
    pub video_custom_format: String,
    pub video_live_from_start: bool,
    pub video_wait_for_live: bool,
    /// Record downloaded video ids and skip them next time (--download-archive).
    pub video_use_archive: bool,

    pub discord_webhook: String,
    pub discord_on_finished: bool,
    pub discord_on_failed: bool,
    pub discord_on_queue: bool,
    pub discord_on_disk: bool,
    #[serde(rename = "DiscordDiskGB")]
    pub discord_disk_gb: i32,
    pub discord_daily: bool,
    pub discord_daily_time: String,
    /// The day the last daily summary was sent (YYYY-MM-DD).
    pub discord_daily_sent: String,

    pub torrent_port: i32,
    pub torrent_seed_ratio: f64,
    pub torrent_seed_minutes: i32,
    pub torrent_seed_action: String,
    pub torrent_always_trackers: String,
    pub torrent_skip_patterns: String,
    pub torrent_mark_unfinished: bool,
    pub torrent_watch_folder: String,
    pub torrent_blocklist_url: String,
    pub torrent_enable_dht: bool,
    pub torrent_enable_local_discovery: bool,
    pub torrent_ipv4_only: bool,
    #[serde(rename = "TorrentUploadLimitKBps")]
    pub torrent_upload_limit_kbps: i32,
    #[serde(rename = "TorrentSeedingLimitKBps")]
    pub torrent_seeding_limit_kbps: i32,
    #[serde(rename = "TorrentUseUPnP")]
    pub torrent_use_upnp: bool,
    pub magnet_handler_backup: Option<String>,

    pub remote_enabled: bool,
    pub remote_port: i32,
    pub remote_pin: String,

    /// Where "Check for updates" looks: a GitHub "owner/repo" whose releases carry FluxDM.exe.
    pub update_repo: String,
    pub auto_check_updates: bool,

    /// Fields this version doesn't use (e.g. the .NET build's window position and torrent tuning), kept so
    /// saving doesn't drop them.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl Settings {
    pub fn normalize_appearance(&mut self) {
        self.extra.remove("SteamGridDbApiKey");
        if !matches!(self.background_color.as_str(), "navy" | "black" | "graphite" | "purple" | "forest") {
            self.background_color = "navy".into();
        }
        if !matches!(self.glass.as_str(), "solid" | "frosted" | "clear" | "ghost") {
            self.glass = "ghost".into();
        }
        if self.accent_color.len() != 7 || !self.accent_color.starts_with('#') || !self.accent_color[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
            self.accent_color = "#e9c98f".into();
        }
        self.line_glow = self.line_glow.clamp(20, 100);
    }
}

impl Default for Settings {
    fn default() -> Self {
        let downloads = std::env::var("USERPROFILE").map(|p| format!("{p}\\Downloads")).unwrap_or_else(|_| "C:\\Downloads".into());
        Self {
            columns: HashMap::new(),
            columns_layout_version: 0,
            sidebar_width: 0.0,
            details_height: 0.0,
            show_icons: true,
            lookup_covers_online: true,
            background_color: "navy".into(),
            accent_from_art: true,
            accent_color: "#e9c98f".into(),
            glass: "ghost".into(),
            line_glow: 70,
            art_behind_window: true,
            art_selected_only: true,
            icon_theme: "heat".into(),
            art_strength: 100,
            art_blur: -1,
            card_art_strength: 100,
            card_art_blur: 0,
            banner_strength: 100,
            banner_blur: 0,
            proxy_mode: "System".into(),
            proxy_host: String::new(),
            proxy_port: 8080,
            proxy_username: String::new(),
            proxy_password: String::new(),
            proxy_bypass_local: false,
            existing_file_policy: "Number".into(),
            incomplete_folder: String::new(),
            default_folder: downloads,
            category_subfolders: false,
            default_connections: 8,
            global_speed_limit_kbps: 0,
            max_retries: 30,
            read_timeout_sec: 30,
            queue_retries: 3,
            max_manual_downloads: 3,
            last_add_queue: String::new(),
            user_agent: "FluxDM/2.0".into(),
            clipboard_monitor: true,
            clipboard_extensions: "zip rar 7z gz tgz bz2 tbz2 xz txz zst lz lzma lz4 cab arj tar iso img vhd vhdx wim esd dmg pkg exe msi msix msixbundle appx appxbundle apk xapk apks apkm aab ipa deb rpm appimage flatpak snap jar run bin mp4 mkv avi mov webm m4v wmv flv f4v mpg mpeg ts m2ts mts 3gp ogv vob rmvb mp3 flac wav m4a m4b aac ogg oga opus wma aiff aif ape alac mka pdf epub mobi azw azw3 cbz cbr djvu doc docx xls xlsx ppt pptx odt ods odp rtf psd ai eps tif tiff dng cr2 nef arw safetensors gguf ggml ckpt pth pt onnx h5 tflite pb npz pak vpk nsp xci wbfs rom sav ttf otf woff2 sqlite db csv".into(),
            local_api_enabled: true,
            local_api_port: 7878,
            close_to_tray: true,
            prevent_sleep: true,
            notify_on_complete: true,
            start_with_windows: false,
            startup_asked_at: None,
            theme: "System".into(),
            speed_schedule_enabled: false,
            schedule_days_mask: 127,
            schedule_from: "09:00".into(),
            schedule_to: "23:00".into(),
            schedule_limit_kbps: 1024,
            video_cookies_browser: "Auto".into(),
            default_video_format: "best".into(),
            video_subtitles: "None".into(),
            video_sub_langs: "en.*,ar.*".into(),
            video_auto_subs: false,
            video_embed_meta: true,
            video_sponsorblock: "Off".into(),
            video_cookies_file: String::new(),
            video_custom_format: String::new(),
            video_live_from_start: false,
            video_wait_for_live: false,
            video_use_archive: false,
            discord_webhook: String::new(),
            discord_on_finished: false,
            discord_on_failed: false,
            discord_on_queue: false,
            discord_on_disk: false,
            discord_disk_gb: 10,
            discord_daily: false,
            discord_daily_time: "23:00".into(),
            discord_daily_sent: String::new(),
            torrent_port: 0,
            torrent_seed_ratio: 1.0,
            torrent_seed_minutes: 0,
            torrent_seed_action: "Stop".into(),
            torrent_always_trackers: String::new(),
            torrent_skip_patterns: String::new(),
            torrent_mark_unfinished: false,
            torrent_watch_folder: String::new(),
            torrent_blocklist_url: String::new(),
            torrent_enable_dht: true,
            torrent_enable_local_discovery: true,
            torrent_ipv4_only: false,
            torrent_upload_limit_kbps: 1024,
            torrent_seeding_limit_kbps: 1024,
            torrent_use_upnp: true,
            magnet_handler_backup: None,
            remote_enabled: false,
            remote_port: 7879,
            remote_pin: String::new(),
            update_repo: "MohammadFakih02/fluxdm".into(),
            auto_check_updates: true,
            extra: HashMap::new(),
        }
    }
}

// ------------------------------------------------------------------ stats

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct DayStat {
    pub bytes: i64,
    pub active_seconds: f64,
    pub files: i32,
    pub peak_bps: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct StatsStore {
    pub days: HashMap<String, DayStat>,
}

impl StatsStore {
    pub fn today_key() -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }
    pub fn record(&mut self, bytes: i64, active_seconds: f64, speed: f64) {
        if bytes <= 0 && active_seconds <= 0.0 {
            return;
        }
        let t = self.days.entry(Self::today_key()).or_default();
        t.bytes += bytes.max(0);
        t.active_seconds += active_seconds;
        if speed > t.peak_bps {
            t.peak_bps = speed;
        }
    }
    pub fn file_completed(&mut self) {
        self.days.entry(Self::today_key()).or_default().files += 1;
    }
}

// ------------------------------------------------------------------ categories & formatting

pub const CATEGORIES: [&str; 9] = ["Compressed", "Documents", "Music", "Video", "Programs", "Images", "AI Models", "Torrents", "Other"];

pub fn category_of(file_name: &str) -> &'static str {
    let ext = std::path::Path::new(file_name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    const MAP: [(&str, &str); 7] = [
        ("Compressed", "zip rar 7z gz tgz bz2 xz zst tar cab iso img"),
        ("Documents", "pdf doc docx xls xlsx ppt pptx odt ods txt rtf epub csv md json xml"),
        ("Music", "mp3 wav flac aac ogg oga m4a opus wma aiff"),
        ("Video", "mp4 mkv avi mov webm flv wmv m4v ts mpg mpeg 3gp"),
        ("Programs", "exe msi msix msixbundle appx appxbundle apk xapk dmg deb rpm appimage bat ps1"),
        ("Images", "jpg jpeg png gif webp bmp svg tif tiff heic avif ico psd"),
        ("AI Models", "safetensors gguf ggml ckpt pt pth onnx bin h5 tflite"),
    ];
    if ext.is_empty() {
        return "Other";
    }
    MAP.iter().find(|(_, exts)| exts.split(' ').any(|e| e == ext)).map(|(c, _)| *c).unwrap_or("Other")
}

pub mod fmt {
    pub fn size(bytes: i64) -> String {
        if bytes < 0 {
            return "?".into();
        }
        const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
        let mut v = bytes as f64;
        let mut i = 0;
        while v >= 1024.0 && i < U.len() - 1 {
            v /= 1024.0;
            i += 1;
        }
        if i == 0 {
            format!("{bytes} B")
        } else {
            let s = format!("{v:.2}");
            let s = s.trim_end_matches('0').trim_end_matches('.');
            format!("{s} {}", U[i])
        }
    }

    pub fn speed(bps: f64) -> String {
        if bps < 1.0 { String::new() } else { format!("{}/s", size(bps as i64)) }
    }

    pub fn limit(kbps: i32) -> String {
        if kbps < 0 {
            "No upload".into()
        } else if kbps == 0 {
            "Unlimited".into()
        } else if kbps >= 1024 && kbps % 1024 == 0 {
            format!("{} MB/s", kbps / 1024)
        } else if kbps >= 1024 {
            let s = format!("{:.2}", kbps as f64 / 1024.0);
            format!("{} MB/s", s.trim_end_matches('0').trim_end_matches('.'))
        } else {
            format!("{kbps} KB/s")
        }
    }

    /// "0", "unlimited", "500", "500k", "500 KB/s", "2m", "2.5 MB/s" → KB/s (plain numbers are KB/s).
    pub fn parse_speed(text: &str) -> Option<i32> {
        let mut s = text.trim().to_lowercase().replace(' ', "").replace("/s", "").replace("ps", "");
        if matches!(s.as_str(), "" | "unlimited" | "none" | "off" | "nolimit") {
            return Some(0);
        }
        let mut mult = 1.0;
        if s.ends_with("gb") || s.ends_with('g') {
            mult = 1024.0 * 1024.0;
            s = s.trim_end_matches('b').trim_end_matches('g').into();
        } else if s.ends_with("mb") || s.ends_with('m') {
            mult = 1024.0;
            s = s.trim_end_matches('b').trim_end_matches('m').into();
        } else if s.ends_with("kb") || s.ends_with('k') {
            s = s.trim_end_matches('b').trim_end_matches('k').into();
        }
        let v: f64 = s.parse().ok()?;
        if v < 0.0 || !v.is_finite() {
            return None;
        }
        Some((v * mult).round().min(i32::MAX as f64) as i32)
    }

    pub fn eta(seconds: f64) -> String {
        if !seconds.is_finite() || seconds <= 0.0 || seconds > 86400.0 * 30.0 {
            return String::new();
        }
        let t = seconds as u64;
        let (d, h, m, s) = (t / 86400, t % 86400 / 3600, t % 3600 / 60, t % 60);
        if d > 0 { format!("{d}d {h}h") } else if h > 0 { format!("{h}h {m}m") } else if m > 0 { format!("{m}m {s}s") } else { format!("{s}s") }
    }

    pub fn duration(seconds: f64) -> String {
        if seconds < 1.0 {
            return String::new();
        }
        let t = seconds as u64;
        let (d, h, m, s) = (t / 86400, t % 86400 / 3600, t % 3600 / 60, t % 60);
        if d > 0 { format!("{d}d {h}h") } else if t >= 3600 { format!("{}h {m}m", t / 3600) } else if t >= 60 { format!("{}m {s}s", t / 60) } else { format!("{t}s") }
    }

    pub fn ago(when: &super::When) -> String {
        let span = (chrono::Local::now() - *when).num_seconds();
        if span < 10 {
            "just now".into()
        } else if span < 60 {
            format!("{span}s ago")
        } else if span < 3600 {
            format!("{}m ago", span / 60)
        } else if span < 86400 {
            format!("{}h ago", span / 3600)
        } else if span < 86400 * 30 {
            format!("{}d ago", span / 86400)
        } else {
            when.format("%-d %b %Y").to_string()
        }
    }
}

/// Is `now` (minutes since midnight) inside [from, to)? Windows may cross midnight (e.g. 23:00–07:00).
pub fn in_window(from: &str, to: &str, now_min: i32) -> bool {
    let (Some(f), Some(t)) = (parse_hhmm(from), parse_hhmm(to)) else { return false };
    if f == t {
        return false;
    }
    if f < t { now_min >= f && now_min < t } else { now_min >= f || now_min < t }
}

pub fn parse_hhmm(s: &str) -> Option<i32> {
    let mut p = s.trim().split(':');
    let h: i32 = p.next()?.trim().parse().ok()?;
    let m: i32 = p.next().unwrap_or("0").trim().parse().ok()?;
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_speeds() {
        assert_eq!(fmt::parse_speed("0"), Some(0));
        assert_eq!(fmt::parse_speed("750k"), Some(750));
        assert_eq!(fmt::parse_speed("2 MB/s"), Some(2048));
        assert_eq!(fmt::parse_speed("2.5m"), Some(2560));
        assert_eq!(fmt::parse_speed("unlimited"), Some(0));
        assert_eq!(fmt::parse_speed("abc"), None);
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(fmt::size(512), "512 B");
        assert_eq!(fmt::size(1536), "1.5 KB");
        assert_eq!(fmt::size(1024 * 1024 * 3), "3 MB");
        assert_eq!(fmt::limit(2048), "2 MB/s");
        assert_eq!(fmt::limit(1536), "1.5 MB/s");
    }

    #[test]
    fn windows_cross_midnight() {
        assert!(in_window("23:00", "07:00", 23 * 60 + 30));
        assert!(in_window("23:00", "07:00", 60));
        assert!(!in_window("23:00", "07:00", 12 * 60));
        assert!(in_window("09:00", "17:00", 10 * 60));
    }

    #[test]
    fn keeps_unknown_settings() {
        let s: Settings = serde_json::from_str(r#"{"WindowLeft":300,"TorrentEnableUtp":false,"DefaultConnections":12}"#).unwrap();
        assert_eq!(s.default_connections, 12);
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["WindowLeft"], 300);
        assert_eq!(back["TorrentEnableUtp"], false);
        assert_eq!(back["UserAgent"], "FluxDM/2.0");
    }

    #[test]
    fn appearance_defaults_and_pascal_case_round_trip() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        let defaults = serde_json::to_value(&old).unwrap();
        for (key, value) in serde_json::json!({"BackgroundColor":"navy", "AccentFromArt":true, "AccentColor":"#e9c98f", "Glass":"ghost", "LineGlow":70, "ArtBehindWindow":true, "ArtSelectedOnly":true, "LookupCoversOnline":true}).as_object().unwrap() {
            assert_eq!(&defaults[key], value);
        }
        let input = serde_json::json!({"BackgroundColor":"forest", "AccentFromArt":false, "AccentColor":"#8fd8ff", "Glass":"clear", "LineGlow":20, "ArtBehindWindow":false, "ArtSelectedOnly":false, "LookupCoversOnline":false, "WindowLeft":123});
        let mut settings: Settings = serde_json::from_value(input.clone()).unwrap();
        settings.normalize_appearance();
        let saved = serde_json::to_value(&settings).unwrap();
        for (key, value) in input.as_object().unwrap() {
            assert_eq!(&saved[key], value);
        }
        let loaded: Settings = serde_json::from_value(saved).unwrap();
        assert_eq!(loaded.background_color, "forest");
        assert!(!loaded.lookup_covers_online);
    }

    #[test]
    fn appearance_invalid_values_fall_back_and_glow_is_bounded() {
        let mut settings = Settings { background_color: "invalid".into(), glass: "invalid".into(), accent_color: "#ééé".into(), line_glow: -1, ..Default::default() };
        settings.normalize_appearance();
        assert_eq!(settings.background_color, "navy");
        assert_eq!(settings.glass, "ghost");
        assert_eq!(settings.accent_color, "#e9c98f");
        assert_eq!(settings.line_glow, 20);
        settings.line_glow = 101;
        settings.normalize_appearance();
        assert_eq!(settings.line_glow, 100);
    }

    #[test]
    fn reads_dotnet_json() {
        let json = r#"{"Id":"0f8fad5b-d9cb-469f-a165-70867728950e","Url":"https://x/y.zip","FileName":"y.zip","SaveFolder":"C:\\D",
            "TotalSize":100,"Downloaded":50,"Status":"Downloading","Connections":8,"SpeedLimitKBps":0,"AddedAt":"2026-09-27T14:03:12.1234567+03:00",
            "LastTry":null,"CompletedAt":"2026-09-27T14:03:12","Segments":[{"Start":0,"End":99,"Downloaded":50}],"Tags":["Games"]}"#;
        let it: Item = serde_json::from_str(json).unwrap();
        assert_eq!(it.status, DlStatus::Downloading);
        assert_eq!(it.saved_segments.len(), 1);
        assert!(it.completed_at.is_some());
        assert_eq!(it.category(), "Compressed");
        let q: QueueDef = serde_json::from_str(r#"{"Name":"Q","OnceDate":"2026-09-27T00:00:00","LastStartFire":"0001-01-01T00:00:00","PostAction":"Shutdown"}"#).unwrap();
        assert_eq!(q.post_action, PostAction::Shutdown);
    }
}
