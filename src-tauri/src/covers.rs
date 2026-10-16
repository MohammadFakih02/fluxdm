//! A picture before each download's name. In order of preference:
//! 1. a picture the user picked, 2. a cover found online from the name (Steam for games; Wikipedia for
//! software, operating systems, emulators and console games such as PS5 packages; Open Library for books;
//! iTunes for movies, TV and albums; Jikan for anime), 3. the file's own icon once it's on disk,
//! 4. otherwise the category symbol. Only the cleaned-up title is sent; a result is only accepted when its
//! words clearly match, and a network failure never counts as "nothing found".

use crate::manager::Manager;
use crate::model::*;
use crate::net;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

pub fn dir() -> PathBuf {
    crate::store::data_dir().join("covers")
}

/// id → (row picture URL, cover URL). Worked out when something changes, not on every refresh.
static URLS: LazyLock<Mutex<HashMap<String, (Option<String>, Option<String>)>>> = LazyLock::new(Default::default);
static ONLINE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static PENDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

pub fn icon_url(it: &Item, _s: &Settings) -> Option<String> {
    URLS.lock().get(&it.id).and_then(|u| u.0.clone())
}

pub fn cover_url(it: &Item, _s: &Settings) -> Option<String> {
    URLS.lock().get(&it.id).and_then(|u| u.1.clone())
}

fn stamp(path: &str) -> u64 {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

fn compute(it: &Item, s: &Settings) -> (Option<String>, Option<String>) {
    if !s.show_icons || it.icon_source == "off" {
        return (None, None);
    }
    let own = (it.icon_source == "user" || it.icon_source.starts_with("steam:") || it.icon_source.starts_with("web:")) && Path::new(&it.icon_file).is_file();
    if own {
        let icon = format!("http://img.localhost/f/{}?v={}", it.id, stamp(&it.icon_file));
        let cover = if Path::new(&it.cover_file).is_file() { Some(format!("http://img.localhost/c/{}?v={}", it.id, stamp(&it.cover_file))) } else { Some(icon.clone()) };
        return (Some(icon), cover);
    }
    if it.status == DlStatus::Completed {
        // A type no app is registered for gets Windows' blank page — FluxDM's category glyph says more,
        // unless the user picked "Use the file's own icon".
        if shell_target(it).is_some_and(|t| it.icon_source == "file" || has_own_icon(&t)) {
            return (Some(format!("http://img.localhost/s/{}", it.id)), None);
        }
    }
    (None, None)
}

/// Programs carry their own icon; other files only have a real one when some app is registered for the type.
fn has_own_icon(path: &str) -> bool {
    let ext = Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "exe" | "msi" | "lnk" | "ico" | "appx" | "msix") {
        return true;
    }
    if ext.is_empty() {
        return false;
    }
    let root = winreg::RegKey::predef(winreg::enums::HKEY_CLASSES_ROOT);
    let Ok(k) = root.open_subkey(format!(".{ext}")) else { return false };
    // Registered with a ProgID (default value) or an OpenWithProgids list = an app handles it.
    k.get_value::<String, _>("").map(|v| !v.is_empty()).unwrap_or(false) || k.open_subkey("OpenWithProgids").is_ok()
}

/// The file whose Windows icon stands for the item (for a finished torrent folder: its installer).
pub fn shell_target(it: &Item) -> Option<String> {
    let p = PathBuf::from(it.full_path());
    if p.is_file() {
        return Some(p.to_string_lossy().into_owned());
    }
    if p.is_dir() {
        let mut exes: Vec<PathBuf> = std::fs::read_dir(&p).ok()?.flatten().map(|e| e.path()).filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))).collect();
        exes.sort_by_key(|f| !f.file_name().map(|n| n.to_string_lossy().to_lowercase().starts_with("setup")).unwrap_or(false));
        return exes.first().map(|f| f.to_string_lossy().into_owned());
    }
    None
}

/// Serves a picture for the `img` protocol: /f/<id> icon file, /c/<id> cover, /s/<id> shell icon.
pub fn serve(mgr: &Manager, path: &str) -> Option<(Vec<u8>, &'static str)> {
    let path = path.trim_start_matches('/');
    let (kind, id) = path.split_once('/')?;
    let it = mgr.item(id)?;
    let mime = |p: &str| match Path::new(p).extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        _ => "image/jpeg",
    };
    match kind {
        "f" => std::fs::read(&it.icon_file).ok().map(|b| (b, mime(&it.icon_file))),
        "c" => std::fs::read(&it.cover_file).ok().map(|b| (b, mime(&it.cover_file))),
        "s" => crate::win::file_icon_png(&shell_target(&it)?).map(|b| (b, "image/png")),
        _ => None,
    }
}

/// Recomputes the pictures for these items, and looks games up on Steam when it's time to.
pub fn refresh_ids(mgr: &Arc<Manager>, ids: &[String]) {
    let s = mgr.settings();
    for id in ids {
        let Some(it) = mgr.item(id) else {
            URLS.lock().remove(id);
            continue;
        };
        URLS.lock().insert(id.clone(), compute(&it, &s));
        if !s.show_icons || it.icon_source == "off" {
            continue;
        }
        // Covers found before: re-fetch the art by the known app id if the file went missing.
        if let Some(app) = it.icon_source.strip_prefix("steam:").and_then(|a| a.parse::<i64>().ok()) {
            if !Path::new(&it.icon_file).is_file() {
                let (m, id) = (mgr.clone(), id.clone());
                tauri::async_runtime::spawn(async move { refetch(&m, &id, app).await });
            }
            continue;
        }
        if it.icon_source.starts_with("web:") {
            if !Path::new(&it.icon_file).is_file() {
                // The picture file is gone: forget the match so the name is searched again.
                mgr.with_item(id, |x| {
                    x.icon_source.clear();
                    x.icon_lookup_at = None;
                });
                mgr.mark_dirty();
            } else {
                continue;
            }
        }
        let it = mgr.item(id).unwrap_or(it);
        if due(&it) && s.lookup_covers_online && worth_lookup(&it) {
            let (m, id) = (mgr.clone(), id.clone());
            tauri::async_runtime::spawn(async move {
                lookup(&m, &id, None).await;
            });
        }
    }
    mgr.push_list();
}

pub fn refresh_all(mgr: &Arc<Manager>) {
    let ids: Vec<String> = mgr.st.lock().items.iter().map(|i| i.id.clone()).collect();
    refresh_ids(mgr, &ids);
}

/// A lookup is due when none ran yet, one failed on the network a while ago, or "nothing found" is a week old.
fn due(it: &Item) -> bool {
    let now = chrono::Local::now();
    match it.icon_source.as_str() {
        "" => it.icon_lookup_at.is_none_or(|t| t < now - chrono::Duration::minutes(10)),
        "none" => it.icon_lookup_at.is_none_or(|t| t < now - chrono::Duration::days(7)),
        _ => false,
    }
}

/// Called every few seconds: an item gets its name late (a magnet resolves, a link's headers arrive),
/// so this picks up anything that is due and not being looked up yet.
pub fn sweep(mgr: &Arc<Manager>) {
    let s = mgr.settings();
    if !s.show_icons || !s.lookup_covers_online {
        return;
    }
    let ids: Vec<String> = {
        let pending = PENDING.lock();
        mgr.st.lock().items.iter().filter(|i| i.icon_source != "off" && due(i) && worth_lookup(i) && !pending.contains(&i.id)).map(|i| i.id.clone()).take(20).collect()
    };
    if !ids.is_empty() {
        refresh_ids(mgr, &ids);
    }
}

fn ext_of(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

const BOOK_EXT: &[&str] = &["epub", "mobi", "azw", "azw3", "djvu", "fb2", "cbz", "cbr", "pdf"];
const CONSOLE_EXT: &[&str] = &["pkg", "nsp", "xci", "nsz", "xcz", "pup", "rap", "3ds", "cia", "wbfs", "rvz", "gcz"];
const OS_EXT: &[&str] = &["iso", "img", "wim", "esd", "vhd", "vhdx", "ova"];

/// Torrents, installers, archives, console packages and books are worth a lookup; videos from sites,
/// pictures, music files and ordinary documents aren't.
fn worth_lookup(it: &Item) -> bool {
    if it.is_video || it.file_name.is_empty() {
        return false;
    }
    if it.is_torrent {
        return true;
    }
    let ext = ext_of(&it.file_name);
    match it.category() {
        "Programs" | "Compressed" => true,
        "Documents" => BOOK_EXT.contains(&ext.as_str()) && significant(&clean_title(&it.file_name)).len() >= 2,
        "Other" => CONSOLE_EXT.contains(&ext.as_str()),
        _ => false,
    }
}

static TITLE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|[^a-z0-9])((?:CUSA|PPSA)\d{5})(?:[^0-9]|$)").unwrap());

/// The PlayStation title ID in a name, upper-cased ("CUSA00362").
fn console_id(name: &str) -> Option<String> {
    TITLE_ID.captures(name).map(|c| c[1].to_uppercase())
}

/// The game's name for a PlayStation title ID (PS4 `CUSA`, PS5 `PPSA`): GarlicSaves' title lookup first,
/// then the patch sites (orbispatches.com for PS4, prosperopatches.com for PS5).
async fn title_from_id(id: &str) -> Option<String> {
    if let Some(doc) = get_json(&format!("https://garlicsaves.com/tools/api/title-search?q={id}")).await {
        let name = doc["results"].as_array().and_then(|r| r.iter().find(|e| e["title_id"].as_str().is_some_and(|t| t.eq_ignore_ascii_case(id)))).and_then(|e| e["name"].as_str());
        if let Some(n) = name.map(tidy_name).filter(|n| n.len() >= 2) {
            return Some(n);
        }
    }
    let host = if id.starts_with("CUSA") { "orbispatches.com" } else { "prosperopatches.com" };
    let r = match net::HTTP.get(format!("https://{host}/{id}")).header("User-Agent", UA).timeout(Duration::from_secs(20)).send().await {
        Ok(r) => r,
        Err(_) => {
            NET_FAILS.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    if !r.status().is_success() {
        return None;
    }
    id_title_from_html(&r.text().await.ok()?, id)
}

/// "Alien: Isolation™" -> "Alien: Isolation" (matching ignores case, so "ASTRO BOT" is fine as it is).
fn tidy_name(n: &str) -> String {
    n.replace(['™', '®'], "").trim().to_string()
}

/// `<title>CUSA00362: Alien: Isolation™ | ORBISPatches.com</title>` -> "Alien: Isolation". The site's
/// "not found" page has a title without the ID, which gives None.
fn id_title_from_html(html: &str, id: &str) -> Option<String> {
    let start = html.find("<title>")? + 7;
    let t = html[start..html.find("</title>")?.max(start)].trim();
    let rest = t.strip_prefix(id)?.trim_start_matches(':').trim();
    let name = rest.rsplit_once(" | ").map(|x| x.0).unwrap_or(rest);
    let name = name.replace("&amp;", "&").replace("&#039;", "'").replace("&#39;", "'").replace("&quot;", "\"").replace(['\u{2122}', '\u{00ae}'], "");
    let name = name.trim().to_string();
    (name.len() >= 2).then_some(name)
}

/// Looks the item up online (by its own name, or by `custom`). Returns the matched title.
pub async fn lookup(mgr: &Arc<Manager>, id: &str, custom: Option<String>) -> Option<String> {
    if !PENDING.lock().insert(id.to_string()) {
        return None;
    }
    let _g = ONLINE.lock().await;
    let result = async {
        let it = mgr.item(id)?;
        let raw = it.file_name.clone();
        let fails_before = NET_FAILS.load(Ordering::Relaxed);
        // A PlayStation title ID ("CUSA00362") says nothing to Steam: look up the game's name first.
        let mut title = custom.clone().unwrap_or_else(|| clean_title(&raw));
        if custom.is_none() {
            if let Some(name) = console_id(&raw) {
                match title_from_id(&name).await {
                    Some(n) => title = n,
                    // No name for the ID and nothing readable left ("EP9000-...-A0118-V0118"): don't search junk.
                    None if !words(&title).iter().any(|w| w.len() >= 4 && w.chars().all(|c| c.is_alphabetic())) => title.clear(),
                    None => {}
                }
            }
        }
        let mut found = None;
        for src in plan(&it, &raw, custom.as_deref()) {
            if let Some(f) = search(src, &title).await {
                found = Some(f);
                break;
            }
        }
        let net_failed = found.is_none() && NET_FAILS.load(Ordering::Relaxed) != fails_before;
        tokio::time::sleep(Duration::from_millis(1500)).await; // stay well within the services' rate limits
        mgr.with_item(id, |it| {
            it.icon_lookup_at = Some(chrono::Local::now());
            match &found {
                Some(f) => {
                    it.icon_source = f.source.clone();
                    it.icon_file = f.icon.clone().or(f.cover.clone()).unwrap_or_default();
                    it.cover_file = f.cover.clone().or(f.icon.clone()).unwrap_or_default();
                }
                // Offline or rate-limited: leave it unmarked so it is tried again in a few minutes.
                None if net_failed => {}
                None if custom.is_none() => it.icon_source = "none".into(),
                None => {}
            }
        });
        mgr.mark_dirty();
        found.map(|f| f.name)
    }
    .await;
    PENDING.lock().remove(id);
    let s = mgr.settings();
    if let Some(it) = mgr.item(id) {
        URLS.lock().insert(id.to_string(), compute(&it, &s));
    }
    mgr.push_list();
    result
}

async fn refetch(mgr: &Arc<Manager>, id: &str, app: i64) {
    if !PENDING.lock().insert(id.to_string()) {
        return;
    }
    let _g = ONLINE.lock().await;
    let (icon, cover) = download_art(app).await;
    if icon.is_some() || cover.is_some() {
        mgr.with_item(id, |it| {
            it.icon_file = icon.clone().or(cover.clone()).unwrap_or_default();
            it.cover_file = cover.unwrap_or_default();
        });
        mgr.mark_dirty();
    }
    PENDING.lock().remove(id);
    let s = mgr.settings();
    if let Some(it) = mgr.item(id) {
        URLS.lock().insert(id.to_string(), compute(&it, &s));
    }
    mgr.push_list();
}

/// Right-click → Icon ▸ Choose image…
pub fn use_image(mgr: &Arc<Manager>, id: &str, path: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let ext = Path::new(path).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dest = dir().join(format!("user-{}{ext}", id.replace('-', ""))).to_string_lossy().into_owned();
    std::fs::copy(path, &dest)?;
    mgr.with_item(id, |it| {
        it.icon_source = "user".into();
        it.icon_file = dest.clone();
        it.cover_file = dest.clone();
    });
    mgr.mark_dirty();
    refresh_ids(mgr, &[id.to_string()]);
    Ok(())
}

pub fn set_mode(mgr: &Arc<Manager>, id: &str, mode: &str) {
    mgr.with_item(id, |it| match mode {
        // "no cover", but keep the file's own icon — and don't look it up again
        "file" => {
            it.icon_source = "none".into();
            it.icon_lookup_at = Some(chrono::Local::now() + chrono::Duration::days(3650));
            it.icon_file.clear();
            it.cover_file.clear();
        }
        "off" => {
            it.icon_source = "off".into();
            it.icon_file.clear();
            it.cover_file.clear();
        }
        _ => {
            it.icon_source.clear();
            it.icon_file.clear();
            it.icon_lookup_at = None;
        }
    });
    mgr.mark_dirty();
    refresh_ids(mgr, &[id.to_string()]);
}

// ------------------------------------------------------------------ titles

const NOISE: &[&str] = &[
    "repack", "repacks", "fitgirl", "dodi", "elamigos", "gog", "codex", "plaza", "skidrow", "empress", "tenoke", "rune", "flt", "razor1911",
    "kaos", "xatab", "steamrip", "portable", "setup", "installer", "x64", "x86", "win64", "win32", "incl", "crack", "cracked", "proper",
    "repacked", "multi", "iso", "mr", "diogenes",
    // console packages, books, releases
    "ps5", "ps4", "ps3", "pkg", "nsp", "xci", "backport", "fw", "libgen", "zlib", "retail", "ebook", "ebooks", "pdf", "epub", "mobi", "azw3",
    "1080p", "720p", "2160p", "480p", "4k", "uhd", "bluray", "brrip", "bdrip", "webrip", "web-dl", "webdl", "hdtv", "dvdrip", "remux", "x264",
    "x265", "h264", "h265", "hevc", "avc", "aac", "ac3", "dts", "ddp5", "ddp", "atmos", "hdr", "hdr10", "10bit", "yify", "yts", "rarbg", "ntb",
    "tgx", "flux", "evo", "cmrg", "sparks", "galaxyrg", "amzn", "nf", "dsnp", "hmax", "imax", "unrated", "extended", "amd64", "arm64", "preactivated",
    "activated", "x86_64", "netinst",
];
const FILLER: &[&str] = &[
    "the", "of", "and", "a", "an", "edition", "deluxe", "ultimate", "complete", "definitive", "goty", "game", "year", "enhanced", "remastered",
    "collection", "gold", "premium", "standard", "digital", "bundle", "dlc", "dlcs", "all", "update", "build", "hotfix", "bonus", "content", "ost",
];
const ALIASES: &[(&str, &str)] = &[
    ("re", "resident evil"), ("gta", "grand theft auto"), ("rdr", "red dead redemption"), ("cod", "call of duty"),
    ("tlou", "the last of us"), ("nfs", "need for speed"), ("mgs", "metal gear solid"), ("ff", "final fantasy"),
];
const ARCHIVE_EXT: &[&str] = &[
    ".rar", ".zip", ".7z", ".iso", ".exe", ".msi", ".tar", ".gz", ".bin", ".img", ".pkg", ".nsp", ".xci", ".dmg", ".apk", ".epub", ".pdf", ".mobi",
    ".azw3", ".djvu", ".cbz", ".cbr", ".wim", ".esd", ".mkv", ".mp4", ".avi",
];
static CONSOLE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:ppsa|cusa|npub|npeb|bcus|bles|blus|sles|slus|npua)\d{3,6}(?:[_-]\d+)?(?:\b|_)").unwrap());
static AUDIO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:ddp|dd|aac|ac3|dts|eac3)[ ._]?[257][ ._][01]\b").unwrap());
static MEDIA_TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bs\d{1,2}e\d{1,3}\b|\bs\d{1,2}\b|\bh ?26[45]\b|\b[257] [01]\b").unwrap());

static PART: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.part\d+$").unwrap());
static BRACKETED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]|\([^)]*\)|\{[^}]*\}").unwrap());
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|[^a-z])v\d+(?:[._]\d+)*[a-z]?\b|\b\d+(?:[._]\d+){2,}\b|\bbuild[ ._]?\d+\b|\bupdate[ ._]?\d+(?:[._]\d+)*\b").unwrap()
});
static NON_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").unwrap());
static SIDE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(soundtrack|ost|demo|dlc|season pass|artbook|art book|playtest|dedicated server|sdk|editor|wallpaper)\b").unwrap());

fn noise(w: &str) -> bool {
    NOISE.contains(&w.to_lowercase().as_str())
}

/// "RE_8_Village_-_Spinneret_Repacks.rar" → "RE 8 Village"; "Game Name [FitGirl Repack]" → "Game Name".
pub fn clean_title(name: &str) -> String {
    let mut s = name.trim().to_string();
    for ext in ARCHIVE_EXT {
        if s.to_lowercase().ends_with(ext) {
            s.truncate(s.len() - ext.len());
            break;
        }
    }
    s = PART.replace(&s, "").into_owned();
    s = BRACKETED.replace_all(&s, " ").into_owned();
    s = VERSION.replace_all(&s, "$1 ").into_owned();
    s = CONSOLE_ID.replace_all(&s, " ").into_owned();
    s = AUDIO.replace_all(&s, " ").into_owned();
    s = s.replace(['_', '.', '+'], " ");
    s = MEDIA_TAGS.replace_all(&s, " ").into_owned();
    // "Game - Group Repacks" / "Game-CODEX": drop a trailing part that's only release-group noise.
    if let Some(dash) = s.rfind(" - ") {
        if dash > 0 && words(&s[dash + 3..]).iter().any(|w| noise(w)) {
            s.truncate(dash);
        }
    }
    if let Some(h) = s.rfind('-') {
        if h > 0 && noise(s[h + 1..].trim()) {
            s.truncate(h);
        }
    }
    let kept: Vec<&str> = s.split(' ').filter(|w| !w.is_empty() && !noise(w.trim_matches('-'))).collect();
    kept.join(" ").trim_matches(|c| c == ' ' || c == '-').to_string()
}

fn words(s: &str) -> Vec<String> {
    NON_WORD.split(&s.to_lowercase()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Title words that matter for matching (aliases expanded, filler dropped).
fn significant(s: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for w in words(s) {
        if let Some((_, full)) = ALIASES.iter().find(|(a, _)| *a == w) {
            out.extend(words(full).into_iter().filter(|x| !FILLER.contains(&x.as_str())));
        } else if !FILLER.contains(&w.as_str()) {
            out.push(w);
        }
    }
    let mut seen = HashSet::new();
    out.retain(|w| seen.insert(w.clone()));
    out
}

/// Searches Steam for the title: (app id, name), or None when nothing matches closely enough.
async fn find(title: &str) -> Option<(i64, String)> {
    let q = significant(title);
    if q.is_empty() || q.concat().len() < 3 {
        return None;
    }
    let ws: Vec<String> = words(title).into_iter().flat_map(|w| match ALIASES.iter().find(|(a, _)| *a == w) { Some((_, f)) => words(f), None => vec![w] }).collect();
    let mut best = best_match(&ws.join(" "), &q, title).await;
    // Steam finds nothing for a number it doesn't know ("resident evil 8 village"): try without bare numbers.
    let no_num: Vec<String> = ws.iter().filter(|w| !w.chars().all(|c| c.is_ascii_digit())).cloned().collect();
    if best.is_none() && no_num.len() >= 2 && no_num.len() < ws.len() {
        best = best_match(&no_num.join(" "), &q, title).await;
    }
    best
}

/// Counts requests that failed for network reasons (not "no result"), so a lookup can tell offline from "not found".
static NET_FAILS: AtomicU32 = AtomicU32::new(0);
const UA: &str = "FluxDM/2.0 (download manager; cover lookup)";

async fn get_json(url: &str) -> Option<Value> {
    let r = match net::HTTP.get(url).header("User-Agent", UA).timeout(Duration::from_secs(20)).send().await {
        Ok(r) => r,
        Err(_) => {
            NET_FAILS.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    if r.status().as_u16() == 429 || r.status().is_server_error() {
        NET_FAILS.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    r.json().await.ok()
}

async fn best_match(term: &str, q: &[String], title: &str) -> Option<(i64, String)> {
    let enc: String = url::form_urlencoded::byte_serialize(term.as_bytes()).collect();
    let doc = get_json(&format!("https://store.steampowered.com/api/storesearch/?l=english&cc=US&term={enc}")).await?;
    let mut best = None;
    let mut best_score = 0.0;
    for e in doc["items"].as_array()?.iter().take(10) {
        let name = e["name"].as_str().unwrap_or("");
        let Some(id) = e["id"].as_i64() else { continue };
        if name.is_empty() || (SIDE.is_match(name) && !SIDE.is_match(title)) {
            continue; // soundtracks, demos, DLC share the game's name
        }
        let r = significant(name);
        if r.is_empty() {
            continue;
        }
        let extra = r.iter().filter(|w| !q.contains(w)).count();
        let missing: Vec<&String> = q.iter().filter(|w| !r.contains(w)).collect();
        let score = if missing.is_empty() && (extra == 0 || (extra == 1 && q.len() >= 2)) {
            2.0 - extra as f64 * 0.1
        } else if extra == 0 && r.len() as f64 >= q.len() as f64 * 0.6 && (r.len() >= 3 || !missing.iter().any(|w| w.chars().all(|c| c.is_ascii_digit()))) {
            1.0 + r.len() as f64 / q.len() as f64 * 0.5
        } else {
            continue; // not close enough — no cover beats a wrong one
        };
        if score > best_score {
            best_score = score;
            best = Some((id, name.to_string()));
        }
    }
    best
}

/// The game's square icon (for the list row) and its cover (for the bigger views).
async fn download_art(app: i64) -> (Option<String>, Option<String>) {
    let _ = std::fs::create_dir_all(dir());
    (icon(app).await, cover(app).await)
}

async fn icon(app: i64) -> Option<String> {
    let dest = dir().join(format!("steam-{app}-icon.jpg"));
    if dest.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Some(dest.to_string_lossy().into_owned());
    }
    let doc = get_json(&format!("https://api.steampowered.com/ICommunityService/GetApps/v1/?appids%5B0%5D={app}")).await?;
    let hash = doc["response"]["apps"].as_array()?.iter().find_map(|a| a["icon"].as_str().filter(|h| !h.is_empty()).map(str::to_string))?;
    save_image(&format!("https://shared.fastly.steamstatic.com/community_assets/images/apps/{app}/{hash}.jpg"), &dest).await
}

/// The portrait library cover where Steam has one; otherwise the store banner.
async fn cover(app: i64) -> Option<String> {
    let dest = dir().join(format!("steam-{app}-cover.jpg"));
    if dest.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Some(dest.to_string_lossy().into_owned());
    }
    if let Some(p) = save_image(&format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{app}/library_600x900.jpg"), &dest).await {
        return Some(p);
    }
    let doc = get_json(&format!("https://store.steampowered.com/api/appdetails?appids={app}&filters=basic")).await?;
    let data = &doc[app.to_string()]["data"];
    for key in ["header_image", "capsule_image"] {
        if let Some(u) = data[key].as_str().filter(|u| !u.is_empty()) {
            if let Some(p) = save_image(u, &dest).await {
                return Some(p);
            }
        }
    }
    None
}

async fn save_image(url: &str, dest: &Path) -> Option<String> {
    let r = match net::HTTP.get(url).header("User-Agent", UA).timeout(Duration::from_secs(20)).send().await {
        Ok(r) => r,
        Err(_) => {
            NET_FAILS.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    let ok_type = r.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("image/"));
    if !r.status().is_success() || !ok_type {
        return None;
    }
    let b = r.bytes().await.ok()?;
    if b.len() < 300 {
        return None;
    }
    std::fs::write(dest, &b).ok()?;
    Some(dest.to_string_lossy().into_owned())
}

// ------------------------------------------------------------------ other sources

#[derive(Clone, Copy, PartialEq, Debug)]
enum Src {
    Steam,
    Game,
    Software,
    Emulator,
    Os,
    Book,
    Movie,
    Tv,
    Album,
    Anime,
}

struct Found {
    source: String,
    name: String,
    icon: Option<String>,
    cover: Option<String>,
}

const EMULATORS: &[&str] = &[
    "ryujinx", "yuzu", "suyu", "sudachi", "citron", "dolphin", "rpcs3", "ppsspp", "pcsx2", "pcsx", "duckstation", "cemu", "xenia", "xemu", "citra", "lime3ds",
    "melonds", "desmume", "mgba", "visualboyadvance", "snes9x", "bsnes", "higan", "ares", "mesen", "nestopia", "fceux", "retroarch", "mame", "flycast",
    "redream", "vita3k", "shadps4", "yabause", "mednafen", "epsxe", "bizhawk", "openemu", "dosbox", "scummvm", "winuae", "fs-uae", "emulator",
];
const OSES: &[&str] = &[
    "windows", "ubuntu", "debian", "fedora", "linux", "mint", "manjaro", "kali", "macos", "opensuse", "freebsd", "tails", "zorin", "elementary", "popos",
    "centos", "rocky", "almalinux", "arch", "endeavouros", "garuda", "nixos", "chromeos", "tinycore", "puppy", "parrot", "bazzite", "steamos", "proxmox",
    "truenas", "pfsense", "opnsense", "android-x86", "bliss", "hiren", "hirens", "gparted", "clonezilla",
];
static HAS_GAME_WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(ps5|ps4|ps3|ps2|psp|vita|switch|xbox|wii|nds|3ds|gamecube|rom)\b|(?:^|[^a-z0-9])(?:ppsa|cusa)\d{5}").unwrap());
static MEDIA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bs\d{1,2}e\d{1,3}\b|\b(1080p|720p|2160p|480p|bluray|brrip|bdrip|webrip|web-dl|webdl|hdtv|dvdrip|remux|x264|x265|hevc)\b").unwrap());
static SEASON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bs\d{1,2}e\d{1,3}\b|\bseason\b|\bcomplete series\b|\bs\d{1,2}\b").unwrap());
static MUSIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(flac|discography|album|320kbps|320 kbps|v0|lossless|mp3)\b").unwrap());
static ANIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\[[^\]]+\]|\b(anime|dual audio|bd rip|bdrip|batch|subsplease|erai-raws|judas)\b").unwrap());
static BOOKISH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(ebook|ebooks|isbn|paperback|hardcover|libgen|z-lib|zlib|book|novel)\b").unwrap());

/// Which sources to try for this item, best guess first. The last entries are fallbacks.
fn plan(it: &Item, raw: &str, custom: Option<&str>) -> Vec<Src> {
    let low = format!("{} {}", raw.to_lowercase(), custom.unwrap_or("").to_lowercase());
    let ext = ext_of(raw);
    let toks: HashSet<String> = words(&low).into_iter().collect();
    let has = |list: &[&str]| list.iter().any(|w| toks.contains(*w));
    if BOOK_EXT.contains(&ext.as_str()) && it.category() == "Documents" || BOOKISH.is_match(&low) {
        return vec![Src::Book];
    }
    if has(EMULATORS) {
        return vec![Src::Emulator, Src::Software];
    }
    if has(OSES) && (OS_EXT.contains(&ext.as_str()) || has(&["iso", "x64", "amd64", "dvd", "netinst", "live", "desktop", "arm64", "lts"])) {
        return vec![Src::Os];
    }
    if CONSOLE_EXT.contains(&ext.as_str()) || HAS_GAME_WORDS.is_match(&low) {
        return vec![Src::Steam, Src::Game];
    }
    if MEDIA.is_match(&low) {
        let mut v = if SEASON.is_match(&low) { vec![Src::Tv, Src::Movie] } else { vec![Src::Movie, Src::Tv] };
        if ANIME.is_match(raw) {
            v.insert(0, Src::Anime);
        }
        return v;
    }
    if MUSIC.is_match(&low) {
        return vec![Src::Album];
    }
    if ANIME.is_match(raw) && it.is_torrent {
        return vec![Src::Anime, Src::Tv];
    }
    vec![Src::Steam, Src::Game, Src::Software]
}

async fn search(src: Src, title: &str) -> Option<Found> {
    match src {
        Src::Steam => {
            let (app, name) = find(title).await?;
            let (icon, cover) = download_art(app).await;
            (icon.is_some() || cover.is_some()).then(|| Found { source: format!("steam:{app}"), name, icon, cover })
        }
        Src::Tv => {
            if let Some((name, img)) = tvmaze(title).await {
                return web_found(src, name, &img).await;
            }
            let (name, img) = wiki(title, src).await?;
            web_found(src, name, &img).await
        }
        Src::Game | Src::Software | Src::Emulator | Src::Os | Src::Movie => {
            let (name, img) = wiki(title, src).await?;
            web_found(src, name, &img).await
        }
        Src::Book => {
            let (name, img) = open_library(title).await?;
            web_found(src, name, &img).await
        }
        Src::Album => {
            let (name, img) = itunes(title, src).await?;
            web_found(src, name, &img).await
        }
        Src::Anime => {
            let (name, img) = jikan(title).await?;
            web_found(src, name, &img).await
        }
    }
}

async fn web_found(src: Src, name: String, img: &str) -> Option<Found> {
    let _ = std::fs::create_dir_all(dir());
    let key = format!("{src:?}:{name}");
    let stem = format!("web-{}", &hex::encode(<sha1::Sha1 as sha1::Digest>::digest(key.as_bytes()))[..16]);
    let path = save_web(img, &stem).await?;
    Some(Found { source: format!("web:{src:?}:{name}"), name, icon: Some(path.clone()), cover: Some(path) })
}

/// Saves a picture as <stem>.<ext>, the extension taken from what the server says it sent.
async fn save_web(url: &str, stem: &str) -> Option<String> {
    for ext in ["png", "jpg", "webp", "gif"] {
        let p = dir().join(format!("{stem}.{ext}"));
        if p.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    let r = match net::HTTP.get(url).header("User-Agent", UA).timeout(Duration::from_secs(20)).send().await {
        Ok(r) => r,
        Err(_) => {
            NET_FAILS.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let ext = if ct.contains("png") {
        "png"
    } else if ct.contains("jpeg") || ct.contains("jpg") {
        "jpg"
    } else if ct.contains("webp") {
        "webp"
    } else if ct.contains("gif") {
        "gif"
    } else {
        return None;
    };
    if !r.status().is_success() {
        return None;
    }
    let b = r.bytes().await.ok()?;
    if b.len() < 300 {
        return None;
    }
    let dest = dir().join(format!("{stem}.{ext}"));
    std::fs::write(&dest, &b).ok()?;
    Some(dest.to_string_lossy().into_owned())
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

const WEB_FILLER: &[&str] = &[
    "pro", "home", "enterprise", "lts", "lite", "desktop", "live", "server", "full", "free", "final", "version", "software", "windows", "bit", "x64", "x86",
    "download", "setup", "crack", "keygen", "patch", "update", "edition", "plus", "studio", "suite", "release",
];

/// Words that identify a title for the non-Steam sources: no numbers (years, versions), no filler.
fn plain(s: &str, drop_windows: bool) -> Vec<String> {
    let mut out: Vec<String> = significant(s).into_iter().filter(|w| !w.chars().all(|c| c.is_ascii_digit())).collect();
    out.retain(|w| !(WEB_FILLER.contains(&w.as_str()) && !(w == "windows" && !drop_windows)) && !noise(w));
    out.retain(|w| w.chars().count() > 1);
    if out.is_empty() {
        out = words(s).into_iter().filter(|w| !w.chars().all(|c| c.is_ascii_digit())).collect();
    }
    out
}

fn strip_parens(s: &str) -> String {
    BRACKETED.replace_all(s, " ").trim().to_string()
}

/// Is `result` the thing the query names? Every word of the result must be in the query (the filename
/// may carry more: an author, a version) and the result must cover `min_cover` of what's asked.
fn name_matches(q: &[String], result: &str, min_cover: f64) -> Option<usize> {
    let r = plain(&strip_parens(result), false);
    if r.is_empty() {
        return None;
    }
    if q.len() >= 2 && r.len() <= q.len() + 1 && q.iter().all(|w| r.contains(w)) {
        return Some(q.len()); // the file name is the result's title minus a prefix ("Marvel's ...")
    }
    if !r.iter().all(|w| q.contains(w)) {
        return None;
    }
    let covered = r.len() as f64 / q.len().max(1) as f64;
    if covered < min_cover || (r.len() == 1 && q.len() > 2) {
        return None;
    }
    Some(r.len())
}

/// The words worth searching for, version numbers kept ("windows 11" must not become "windows").
fn terms(title: &str) -> String {
    let w: Vec<String> = words(title).into_iter().filter(|w| !noise(w) && !(WEB_FILLER.contains(&w.as_str()) && w != "windows")).collect();
    if w.is_empty() { words(title).join(" ") } else { w.join(" ") }
}

/// TVMaze: show posters.
async fn tvmaze(title: &str) -> Option<(String, String)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let doc = get_json(&format!("https://api.tvmaze.com/search/shows?q={}", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, String)> = None;
    for e in doc.as_array()?.iter().take(6) {
        let (Some(name), Some(img)) = (e["show"]["name"].as_str(), e["show"]["image"]["original"].as_str().or(e["show"]["image"]["medium"].as_str())) else { continue };
        let Some(n) = name_matches(&q, name, 0.5) else { continue };
        if best.as_ref().is_none_or(|b| n > b.0) {
            best = Some((n, name.to_string(), img.replace("http://", "https://")));
        }
    }
    best.map(|(_, n, i)| (n, i))
}

/// Wikipedia: logos of software, operating systems and emulators, box art of console games.
async fn wiki(title: &str, kind: Src) -> Option<(String, String)> {
    let q = plain(title, kind != Src::Os);
    if q.is_empty() || q.concat().len() < 3 {
        return None;
    }
    let (hint, accept) = match kind {
        Src::Os => ("operating system", r"(?i)operating system|linux distribution|\bdistribution\b|live (cd|usb)|unix"),
        Src::Emulator => ("emulator", r"(?i)emulator"),
        Src::Game => ("video game", r"(?i)video game|\bgame\b|role-playing|action-adventure|shooter|platform"),
        Src::Movie => ("film", r"(?i)\bfilm\b|\bmovie\b"),
        Src::Tv => ("television series", r"(?i)television|\bseries\b|\bshow\b|sitcom"),
        _ => ("software", r"(?i)software|application|program|suite|editor|browser|player|utility|tool|framework|engine|toolkit|\bapp\b|launcher|client|compiler|ide\b|library"),
    };
    let accept = Regex::new(accept).ok()?;
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&format=json&generator=search&gsrsearch={}&gsrlimit=8&prop=pageimages%7Cextracts&piprop=thumbnail&pilicense=any&pithumbsize=512&exintro=1&explaintext=1&exsentences=2&exlimit=max",
        enc(&format!("{} {hint}", terms(title)))
    );
    let doc = get_json(&url).await?;
    let pages = doc["query"]["pages"].as_object()?;
    let want_digits: Vec<String> = words(title).into_iter().filter(|w| w.chars().all(|c| c.is_ascii_digit()) && w.len() < 4).collect();
    let mut best: Option<(bool, usize, i64, String, String)> = None;
    for pg in pages.values() {
        let (Some(name), Some(img)) = (pg["title"].as_str(), pg["thumbnail"]["source"].as_str()) else { continue };
        if !accept.is_match(pg["extract"].as_str().unwrap_or("")) {
            continue;
        }
        let Some(n) = name_matches(&q, name, 0.5) else { continue };
        let rank = pg["index"].as_i64().unwrap_or(99);
        // "Windows 11" beats "Windows 10" for a Windows 11 file; other things being equal the search order decides.
        let have_digits: Vec<String> = words(name).into_iter().filter(|w| w.chars().all(|c| c.is_ascii_digit()) && w.len() < 4).collect();
        let same = have_digits == want_digits;
        let better = match &best {
            None => true,
            Some(b) => (same, n, -rank) > (b.0, b.1, -b.2),
        };
        if better {
            best = Some((same, n, rank, strip_parens(name), img.to_string()));
        }
    }
    best.map(|b| (b.3, b.4))
}

/// Open Library: book covers by title (the file name usually carries the author too).
async fn open_library(title: &str) -> Option<(String, String)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let doc = get_json(&format!("https://openlibrary.org/search.json?q={}&limit=8&fields=title,cover_i", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, i64)> = None;
    for d in doc["docs"].as_array()? {
        let (Some(name), Some(cover)) = (d["title"].as_str(), d["cover_i"].as_i64()) else { continue };
        let Some(n) = name_matches(&q, name, 0.3) else { continue };
        if n < 2 {
            continue; // a one-word title is too easy to hit by accident
        }
        if best.as_ref().is_none_or(|b| n > b.0) {
            best = Some((n, name.to_string(), cover));
        }
    }
    best.map(|(_, name, cover)| (name, format!("https://covers.openlibrary.org/b/id/{cover}-L.jpg")))
}

/// iTunes search: movie posters, TV season art, album covers.
async fn itunes(title: &str, kind: Src) -> Option<(String, String)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let (media, entity) = match kind {
        Src::Movie => ("movie", "movie"),
        Src::Tv => ("tvShow", "tvSeason"),
        _ => ("music", "album"),
    };
    let doc = get_json(&format!("https://itunes.apple.com/search?term={}&media={media}&entity={entity}&limit=10", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, String)> = None;
    for r in doc["results"].as_array()? {
        let raw = r[if kind == Src::Movie { "trackName" } else { "collectionName" }].as_str().unwrap_or("");
        let name = raw.split(", Season").next().unwrap_or(raw).trim().to_string();
        let Some(img) = r["artworkUrl100"].as_str() else { continue };
        if kind == Src::Album {
            // the artist must be in the file name too, or any album called "Greatest Hits" would match
            let artist = plain(r["artistName"].as_str().unwrap_or(""), true);
            if artist.is_empty() || !artist.iter().any(|w| q.contains(w)) {
                continue;
            }
        }
        let Some(n) = name_matches(&q, &name, 0.5) else { continue };
        if best.as_ref().is_none_or(|b| n > b.0) {
            best = Some((n, name, img.replace("100x100bb", "600x600bb")));
        }
    }
    best.map(|(_, n, i)| (n, i))
}

/// Jikan (MyAnimeList): anime posters.
async fn jikan(title: &str) -> Option<(String, String)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let doc = get_json(&format!("https://api.jikan.moe/v4/anime?q={}&limit=6&sfw=false", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, String)> = None;
    for a in doc["data"].as_array()? {
        let Some(img) = a["images"]["jpg"]["large_image_url"].as_str() else { continue };
        for key in ["title", "title_english"] {
            let Some(name) = a[key].as_str() else { continue };
            if let Some(n) = name_matches(&q, name, 0.5) {
                if best.as_ref().is_none_or(|b| n > b.0) {
                    best = Some((n, name.to_string(), img.to_string()));
                }
            }
        }
    }
    best.map(|(_, n, i)| (n, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("RE_8_Village_-_Spinneret_Repacks.rar"), "RE 8 Village");
        assert_eq!(clean_title("Some Game [FitGirl Repack].rar"), "Some Game");
        assert_eq!(clean_title("Cool.Game.v1.2.3-CODEX.iso"), "Cool Game");
        assert_eq!(significant("RE 8 Village"), vec!["resident", "evil", "8", "village"]);
    }

    #[test]
    fn cleans_other_kinds() {
        assert_eq!(clean_title("Spider-Man 2 [PPSA01234] PS5 PKG v1.00.pkg"), "Spider-Man 2");
        assert_eq!(clean_title("Dune.Part.Two.2024.1080p.WEB-DL.DDP5.1.Atmos.H.264-FLUX"), "Dune Part Two 2024");
        assert_eq!(clean_title("Breaking.Bad.S01E02.720p.BluRay.x264-YIFY"), "Breaking Bad");
        assert_eq!(words(&clean_title("ubuntu-24.04.1-desktop-amd64.iso")), vec!["ubuntu", "desktop"]);
    }

    #[test]
    fn matches_titles_for_other_sources() {
        let q = plain("Windows 11 Pro", false);
        assert_eq!(name_matches(&q, "Windows 11", 0.5), Some(1));
        let q = plain("Adobe Photoshop 2024 Portable", true);
        assert!(name_matches(&q, "Adobe Photoshop", 0.5).is_some());
        assert!(name_matches(&q, "Adobe Illustrator", 0.5).is_none());
        let q = plain("Clean Code Robert C Martin", true);
        assert!(name_matches(&q, "Clean Code", 0.3).is_some());
        assert!(name_matches(&plain("Dolphin Emulator Setup", true), "Dolphin (emulator)", 0.5).is_some());
        assert!(name_matches(&plain("Spider-Man 2", true), "Marvel's Spider-Man 2", 0.5).is_some());
        assert!(name_matches(&plain("Spider-Man 2", true), "Marvel's Spider-Man 2: Miles Morales Edition Extra", 0.5).is_none());
    }

    #[test]
    fn reads_playstation_title_ids() {
        assert_eq!(console_id("Alien Isolation [cusa00362] PS4.pkg").as_deref(), Some("CUSA00362"));
        assert_eq!(console_id("UP0001-PPSA01342_00-GAME.pkg").as_deref(), Some("PPSA01342"));
        assert_eq!(console_id("Elden Ring"), None);
        let page = "<head>\n\t\t<title>CUSA00362: Alien: Isolation\u{2122} | ORBISPatches.com</title>";
        assert_eq!(id_title_from_html(page, "CUSA00362").as_deref(), Some("Alien: Isolation"));
        assert_eq!(tidy_name("ASTRO BOT"), "ASTRO BOT");
        assert_eq!(tidy_name("Alien: Isolation\u{2122}"), "Alien: Isolation");
        assert_eq!(console_id("EP9000-PPSA21567_00-0000000000000000-A0118-V0118").as_deref(), Some("PPSA21567"));
        assert_eq!(id_title_from_html("<title>Patch Installer | ORBISPatches.com</title>", "CUSA99999"), None);
    }

    fn item(name: &str, torrent: bool) -> Item {
        let mut it = Item::default();
        it.file_name = name.into();
        it.is_torrent = torrent;
        it
    }

    #[test]
    fn plans_by_kind() {
        let p = |n: &str, t: bool| plan(&item(n, t), n, None);
        assert_eq!(p("Spider-Man 2 [PPSA01234] PS5.pkg", false), vec![Src::Steam, Src::Game]);
        assert_eq!(p("Clean Code - Robert Martin.epub", false), vec![Src::Book]);
        assert_eq!(p("ubuntu-24.04-desktop-amd64.iso", false), vec![Src::Os]);
        assert_eq!(p("Dolphin-x64-2412.exe", false), vec![Src::Emulator, Src::Software]);
        assert_eq!(p("Breaking.Bad.S01E02.720p.BluRay.x264", true), vec![Src::Tv, Src::Movie]);
        assert_eq!(p("Dune.2021.1080p.BluRay.x264", true), vec![Src::Movie, Src::Tv]);
        assert_eq!(p("Elden Ring [FitGirl Repack]", true), vec![Src::Steam, Src::Game, Src::Software]);
    }

    /// Hits the real services: `cargo test --bin fluxdm live_lookup -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_lookup() {
        let cases: &[(Src, &str)] = &[
            (Src::Steam, "Elden Ring"),
            (Src::Game, "Spider-Man 2"),
            (Src::Software, "Blender"),
            (Src::Software, "Adobe Photoshop"),
            (Src::Emulator, "Dolphin"),
            (Src::Emulator, "RPCS3"),
            (Src::Os, "Ubuntu desktop"),
            (Src::Os, "Windows 11 Pro"),
            (Src::Book, "Clean Code Robert C Martin"),
            (Src::Movie, "Dune Part Two 2024"),
            (Src::Tv, "Breaking Bad"),
            (Src::Album, "AC DC Back in Black"),
            (Src::Anime, "Attack on Titan"),
        ];
        let id = title_from_id("CUSA00362").await;
        println!("CUSA00362 -> {id:?}");
        let ab = title_from_id("PPSA21567").await;
        println!("PPSA21567 -> {ab:?}");
        if let Some(n) = ab {
            println!("  Astro then Game -> {:?}", search(Src::Game, &n).await.map(|f| f.source));
        }
        if let Some(n) = id {
            println!("  then Steam -> {:?}", search(Src::Steam, &n).await.map(|f| f.source));
        }
        for (src, title) in cases {
            let r = search(*src, title).await;
            println!("{src:?} {title:?} -> {:?}", r.map(|f| (f.name, f.source)));
        }
    }}
