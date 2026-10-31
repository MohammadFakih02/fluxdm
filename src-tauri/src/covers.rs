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

/// id → (row picture URL, cover URL, banner URL). Worked out when something changes, not on every refresh.
type ArtUrls = (Option<String>, Option<String>, Option<String>);
static URLS: LazyLock<Mutex<HashMap<String, ArtUrls>>> = LazyLock::new(Default::default);
static ONLINE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static PENDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

pub fn icon_url(it: &Item, _s: &Settings) -> Option<String> {
    URLS.lock().get(&it.id).and_then(|u| u.0.clone())
}

pub fn cover_url(it: &Item, _s: &Settings) -> Option<String> {
    URLS.lock().get(&it.id).and_then(|u| u.1.clone())
}

pub fn is_game(it: &Item) -> bool {
    it.icon_source.starts_with("steam:") || it.icon_source.starts_with("web:Game:") || console_id(&it.file_name).is_some()
}

pub fn banner_url(it: &Item, _s: &Settings) -> Option<String> {
    URLS.lock().get(&it.id).and_then(|u| u.2.clone())
}

fn stamp(path: &str) -> u64 {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

fn compute(it: &Item, s: &Settings) -> ArtUrls {
    if !s.show_icons || it.icon_source == "off" {
        return (None, None, None);
    }
    let banner = (Path::new(&it.banner_file).is_file()).then(|| format!("http://img.localhost/b/{}?v={}{}", it.id, stamp(&it.banner_file), if it.banner_file.contains("web-shot-") { "&texture=1" } else { "" }));
    let own = (it.icon_source == "user" || it.icon_source.starts_with("steam:") || it.icon_source.starts_with("web:")) && Path::new(&it.icon_file).is_file();
    if own {
        let icon = format!("http://img.localhost/f/{}?v={}", it.id, stamp(&it.icon_file));
        let cover = if Path::new(&it.cover_file).is_file() { Some(format!("http://img.localhost/c/{}?v={}", it.id, stamp(&it.cover_file))) } else { Some(icon.clone()) };
        return (Some(icon), cover, banner);
    }
    if it.status == DlStatus::Completed {
        // A type no app is registered for gets Windows' blank page — FluxDM's category glyph says more,
        // unless the user picked "Use the file's own icon".
        if shell_target(it).is_some_and(|t| it.icon_source == "file" || has_own_icon(&t)) {
            return (Some(format!("http://img.localhost/s/{}", it.id)), None, banner);
        }
    }
    (None, None, banner)
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

/// Serves a picture for the `img` protocol: /f/<id> icon file, /c/<id> cover, /b/<id> banner, /s/<id> shell icon.
pub fn serve(mgr: &Manager, path: &str) -> Option<(Vec<u8>, &'static str)> {
    let path = path.trim_start_matches('/');
    let (kind, id) = path.split_once('/')?;
    if kind == "p" {
        // Add previews only serve validated artwork cache names, never arbitrary paths.
        if !(id.starts_with("steam-") || id.starts_with("web-")) || id.contains("..") || ![".jpg", ".png", ".webp", ".gif"].iter().any(|ext| id.ends_with(ext)) || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
            return None;
        }
        return std::fs::read(dir().join(id)).ok().map(|b| (b, if id.ends_with(".png") { "image/png" } else if id.ends_with(".webp") { "image/webp" } else if id.ends_with(".gif") { "image/gif" } else { "image/jpeg" }));
    }
    let it = mgr.item(id)?;
    match kind {
        "f" => std::fs::read(&it.icon_file).ok().map(|b| (b, image_mime(&it.icon_file))),
        "c" => std::fs::read(&it.cover_file).ok().map(|b| (b, image_mime(&it.cover_file))),
        "b" => std::fs::read(&it.banner_file).ok().map(|b| (b, image_mime(&it.banner_file))),
        "s" => crate::win::file_icon_png(&shell_target(&it)?).map(|b| (b, "image/png")),
        _ => None,
    }
}

fn image_mime(p: &str) -> &'static str {
    match Path::new(p).extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        _ => "image/jpeg",
    }
}

/// Preview verified game or program artwork before adding a download.
pub async fn preview(name: &str, link: &str, headers: &str) -> Option<Value> {
    type PreviewCache = HashMap<String, (std::time::Instant, Option<Value>)>;
    static CACHE: LazyLock<Mutex<PreviewCache>> = LazyLock::new(Default::default);
    let title = clean_title(&name.chars().take(300).collect::<String>());
    let cache_key = format!("{title}:{}", hash_stem(&format!("{link}:{headers}")));
    if title.len() < 3 { return None; }
    let _guard = ONLINE.lock().await;
    if let Some((at, value)) = CACHE.lock().get(&cache_key) {
        if at.elapsed() < Duration::from_secs(300) { return value.clone(); }
    }
    let result = async {
        let title = if let Some(id) = console_id(name) { title_from_id(&id).await.unwrap_or(title.clone()) } else { title.clone() };
        let it = Item { file_name: name.to_string(), url:link.to_string(), headers:headers.to_string(), ..Item::default() };
        for src in if vendor(&it.file_name).is_some() { vec![Src::Software] } else { plan(&it, name, None) } {
            if let Some(f) = search(src, &title).await {
                let f = enrich_art(&it, &title, Some(f)).await?;
                let url = |path: Option<String>| path.and_then(|p| Path::new(&p).file_name().map(|n| format!("http://img.localhost/p/{}{}", n.to_string_lossy(), if p.contains("web-shot-") { "?texture=1" } else { "" })));
                return Some(serde_json::json!({ "title": if src == Src::Steam || src == Src::Game { f.name } else { title.clone() }, "game": src == Src::Steam || src == Src::Game, "icon": url(f.icon.or(f.cover)), "banner": url(f.banner), "artColor": f.art_color }));
            }
        }
        let f = enrich_art(&it, &title, None).await;
        let url = |p: Option<String>| p.and_then(|p| Path::new(&p).file_name().map(|n| format!("http://img.localhost/p/{}{}", n.to_string_lossy(), if p.contains("web-shot-") { "?texture=1" } else { "" })));
        let display = f.as_ref().map(|f| f.name.clone()).unwrap_or_else(|| title.clone());
        Some(serde_json::json!({ "title": display, "game": f.as_ref().is_some_and(|f| f.source.starts_with("steam:") || f.source.starts_with("web:Game:")), "icon": url(f.as_ref().and_then(|f| f.icon.clone())), "artColor": f.as_ref().map(|f|f.art_color.clone()), "banner": url(f.and_then(|f| f.banner)) }))
    }.await;
    let mut cache = CACHE.lock();
    if cache.len() >= 100 { cache.clear(); }
    cache.insert(cache_key, (std::time::Instant::now(), result.clone()));
    result
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
        if s.lookup_covers_online && console_title_due(&it) {
            let (m, id) = (mgr.clone(), id.clone());
            tauri::async_runtime::spawn(async move { repair_console_title(&m, &id).await; });
            continue;
        }
        if s.lookup_covers_online && artwork_due(&it) {
            let (m, id) = (mgr.clone(), id.clone());
            tauri::async_runtime::spawn(async move {
                if it.icon_source == "none" { lookup(&m, &id, None).await; }
                else { repair_artwork(&m, &id).await; }
            });
            continue;
        }
        // Covers found before: re-fetch the art by the known app id if the file went missing.
        if let Some(app) = it.icon_source.strip_prefix("steam:").and_then(|a| a.parse::<i64>().ok()) {
            if s.lookup_covers_online && repair_due(&it) {
                let (m, id) = (mgr.clone(), id.clone());
                tauri::async_runtime::spawn(async move { refetch(&m, &id, app).await });
            }
            continue;
        }
        if it.icon_source.starts_with("web:") {
            if ![&it.icon_file, &it.cover_file, &it.banner_file].iter().any(|p| Path::new(p).is_file()) {
                // All cached artwork is gone: forget the match so the name is searched again.
                mgr.with_item(id, |x| {
                    x.icon_source.clear();
                    x.icon_lookup_at = None;
                });
                mgr.mark_dirty();
            } else {
                if s.lookup_covers_online && web_banner_due(&it) {
                    let (m, id) = (mgr.clone(), id.clone());
                    tauri::async_runtime::spawn(async move { repair_web_banner(&m, &id).await });
                }
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
    static VALIDATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !VALIDATED.swap(true, Ordering::Relaxed) {
        for id in &ids {
            if let Some(it) = mgr.item(id) {
                if invalid_steam_match(&it) {
                    mgr.with_item(id, reset_steam_match);
                    mgr.mark_dirty();
                }
            }
        }
    }
    refresh_ids(mgr, &ids);
}

fn invalid_steam_match(it: &Item) -> bool {
    it.icon_source.starts_with("steam:") && (it.steam_title.is_empty() || steam_score(&if it.console_title.is_empty() { clean_game_title(&it.file_name) } else { it.console_title.clone() }, &it.steam_title).is_none())
}

fn reset_steam_match(it: &mut Item) {
    it.icon_source.clear(); it.icon_file.clear(); it.cover_file.clear(); it.banner_file.clear();
    it.steam_title.clear(); it.icon_lookup_at = None;
}

fn web_banner_due(it: &Item) -> bool {
    it.icon_source.starts_with("web:") && !Path::new(&it.banner_file).is_file()
        && it.icon_lookup_at.is_none_or(|t| t < chrono::Local::now() - chrono::Duration::days(7))
}

/// A lookup is due when none ran yet, one failed on the network a while ago, or "nothing found" is a week old.
fn console_title_due(it: &Item) -> bool {
    (it.icon_source.starts_with("steam:") || it.icon_source.starts_with("web:"))
        && it.console_title.is_empty() && console_id(&it.file_name).is_some()
        && it.icon_lookup_at.is_none_or(|t| t < chrono::Local::now() - chrono::Duration::minutes(10))
}

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
        mgr.st.lock().items.iter().filter(|i| i.icon_source != "off" && (due(i) || artwork_due(i) || repair_due(i) || web_banner_due(i) || console_title_due(i)) && worth_lookup(i) && !pending.contains(&i.id)).map(|i| i.id.clone()).take(20).collect()
    };
    if !ids.is_empty() {
        refresh_ids(mgr, &ids);
    }
}

fn repair_due(it: &Item) -> bool {
    it.icon_source.starts_with("steam:")
        && (it.banner_file.ends_with("-banner.jpg") || [&it.icon_file, &it.cover_file, &it.banner_file].iter().any(|p| !Path::new(p).is_file()))
        && it.icon_lookup_at.is_none_or(|t| t < chrono::Local::now() - chrono::Duration::minutes(10))
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
        "Other" => CONSOLE_EXT.contains(&ext.as_str()) || OS_EXT.contains(&ext.as_str()),
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
        let mut console_title = None;
        if custom.is_none() {
            if let Some(name) = console_id(&raw) {
                match title_from_id(&name).await {
                    Some(n) => { console_title = Some(n.clone()); title = n; },
                    // No name for the ID and nothing readable left ("EP9000-...-A0118-V0118"): don't search junk.
                    None if !words(&title).iter().any(|w| w.len() >= 4 && w.chars().all(|c| c.is_alphabetic())) => title.clear(),
                    None => {}
                }
            }
        }
        let mut found = None;
        for src in if vendor(&it.file_name).is_some() { vec![Src::Software] } else { plan(&it, &raw, custom.as_deref()) } {
            if let Some(f) = search(src, &title).await {
                found = Some(f);
                break;
            }
        }
        let found = enrich_art(&it, &title, found).await;
        let revision = 16;
        let net_failed = found.is_none() && NET_FAILS.load(Ordering::Relaxed) != fails_before;
        tokio::time::sleep(Duration::from_millis(1500)).await; // stay well within the services' rate limits
        mgr.with_item(id, |it| {
            it.icon_lookup_at = Some(chrono::Local::now());
            it.banner_revision = revision;
            if let Some(title) = &console_title { it.console_title = title.clone(); }
            match &found {
                Some(f) => {
                    it.steam_title = if f.source.starts_with("steam:") { f.name.clone() } else { String::new() };
                    it.icon_source = f.source.clone();
                    it.icon_file = f.icon.clone().or(f.cover.clone()).unwrap_or_default();
                    it.cover_file = f.cover.clone().or(f.icon.clone()).unwrap_or_default();
                    if !is_user_banner(&it.banner_file) { it.banner_file = f.banner.clone().unwrap_or_default(); }
                    it.art_color = f.art_color.clone();
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

async fn repair_console_title(mgr: &Arc<Manager>, id: &str) {
    if !PENDING.lock().insert(id.to_string()) { return; }
    let _g = ONLINE.lock().await;
    if let Some(it) = mgr.item(id) {
        if let Some(code) = console_id(&it.file_name) {
            let title = title_from_id(&code).await;
            mgr.with_item(id, |x| {
                if x.file_name != it.file_name { return; }
                x.icon_lookup_at = Some(chrono::Local::now());
                if let Some(title) = title { x.console_title = title; }
            });
            mgr.mark_dirty();
        }
    }
    PENDING.lock().remove(id);
    mgr.push_list();
}

async fn refetch(mgr: &Arc<Manager>, id: &str, _app: i64) {
    repair_artwork(mgr, id).await;
}

/// Right-click → Icon ▸ Choose image…
pub fn use_image(mgr: &Arc<Manager>, id: &str, path: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let ext = Path::new(path).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dest = dir().join(format!("user-{}{ext}", id.replace('-', ""))).to_string_lossy().into_owned();
    std::fs::copy(path, &dest)?;
    mgr.with_item(id, |it| {
        it.banner_file.clear();
        it.art_color.clear();
        it.icon_source = "user".into();
        it.icon_file = dest.clone();
        it.cover_file = dest.clone();
    });
    mgr.mark_dirty();
    refresh_ids(mgr, &[id.to_string()]);
    Ok(())
}

/// A banner the user picked (right-click → Banner → Choose image…). Automatic lookups never replace it.
fn is_user_banner(path: &str) -> bool {
    Path::new(path).file_name().is_some_and(|n| n.to_string_lossy().starts_with("user-banner-")) && Path::new(path).is_file()
}

/// Uses a picture from disk as the item's banner. Blank padding is cropped off, but the picture is never
/// turned down: it's the user's choice.
pub fn use_banner(mgr: &Arc<Manager>, id: &str, path: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let ext = Path::new(path).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dest = dir().join(format!("user-banner-{}{ext}", id.replace('-', ""))).to_string_lossy().into_owned();
    let _ = std::fs::remove_file(Path::new(&dest).with_file_name(format!("user-banner-{}-fit.png", id.replace('-', ""))));
    std::fs::copy(path, &dest)?;
    let fitted = fit_banner(&dest, false).ok_or_else(|| std::io::Error::other("that file isn't a picture FluxDM can read (PNG, JPEG or WebP)"))?;
    mgr.with_item(id, |it| it.banner_file = fitted.clone());
    mgr.mark_dirty();
    refresh_ids(mgr, &[id.to_string()]);
    Ok(())
}

/// Removes a user-chosen banner; the next automatic lookup finds one again.
pub fn reset_banner(mgr: &Arc<Manager>, id: &str) {
    mgr.with_item(id, |it| {
        if is_user_banner(&it.banner_file) { it.banner_file.clear(); }
        it.icon_lookup_at = None;
        it.banner_revision = 0;
    });
    mgr.mark_dirty();
    refresh_ids(mgr, &[id.to_string()]);
}
pub fn set_mode(mgr: &Arc<Manager>, id: &str, mode: &str) {
    mgr.with_item(id, |it| {
        it.banner_file.clear();
        it.art_color.clear();
        match mode {
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
    let low = name.to_lowercase();
    // Release-group markers precede the general installer and media noise words.
    let game = HAS_GAME_WORDS.is_match(name) || CONSOLE_EXT.contains(&ext_of(name).as_str())
        || NOISE[..17].iter().any(|w| words(&low).iter().any(|t| t == w));
    if game { return clean_game_title(name); }
    let mut title = name.trim().to_string();
    for ext in ARCHIVE_EXT {
        if title.to_lowercase().ends_with(ext) { title.truncate(title.len() - ext.len()); break; }
    }
    let chars: Vec<char> = title.chars().collect();
    chars.iter().enumerate().map(|(i,c)| if *c == '_' || *c == '-' || *c == '.' && !(i > 0 && i+1 < chars.len() && chars[i-1].is_ascii_digit() && chars[i+1].is_ascii_digit()) { ' ' } else { *c }).collect::<String>()
        .split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_game_title(name: &str) -> String {
    let mut s = name.trim().to_string();
    for ext in ARCHIVE_EXT {
        if s.to_lowercase().ends_with(ext) {
            s.truncate(s.len() - ext.len());
            break;
        }
    }
    s = PART.replace(&s, "").into_owned();
    s = s.replace('_', " ");
    static SITE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:[a-z0-9]+[-.])*[a-z0-9]+\.(?:site|com|net|org|to|me|info)\b").unwrap());
    s = SITE.replace_all(&s, " ").into_owned();
    s = BRACKETED.replace_all(&s, " ").into_owned();
    s = s.replace('_', " ");
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
    static PLATFORM_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:[\s.-]+(?:windows|pc|win64|win32|x64|x86|linux|portable))+$").unwrap());
    s = PLATFORM_SUFFIX.replace(s.trim_end(), "").into_owned();
    let kept: Vec<&str> = s.split(' ').filter(|w| w.chars().any(char::is_alphanumeric) && !noise(w.trim_matches('-'))).collect();
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

/// Short or generic queries require the full title, ignoring punctuation and case.
fn steam_score(title: &str, name: &str) -> Option<f64> {
    let q = significant(title); let r = significant(name);
    if !q.iter().any(|w| w.chars().any(|c| c.is_alphabetic())) { return None; }
    if words(title).concat() == words(name).concat() { return Some(3.0); }
    let count = q.iter().filter(|w| w.chars().any(|c| c.is_alphabetic()) && !["setup", "installer", "payload", "deck"].contains(&w.as_str())).count();
    if count < 2 || words(title).len() < 2 || r.is_empty() { return None; }
    let extra = r.iter().filter(|w| !q.contains(w)).count();
    let missing: Vec<&String> = q.iter().filter(|w| !r.contains(w)).collect();
    if missing.is_empty() && (extra == 0 || extra == 1 && q.len() >= 2) { Some(2.0 - extra as f64 * 0.1) }
    else if extra == 0 && r.len() as f64 >= q.len() as f64 * 0.6 && (r.len() >= 3 || !missing.iter().any(|w| w.chars().all(|c| c.is_ascii_digit()))) { Some(1.0 + r.len() as f64 / q.len() as f64 * 0.5) }
    else { None }
}

pub fn display_title(it: &Item) -> String {
    if !it.console_title.is_empty() && console_id(&it.file_name).is_some() { return it.console_title.clone(); }
    if !it.is_video && (OS_EXT.contains(&ext_of(&it.file_name).as_str()) || it.icon_source.starts_with("steam:") || it.icon_source.starts_with("web:Game:") || it.icon_source.starts_with("web:Software:") || matches!(it.category(), "Programs" | "Compressed") || it.is_torrent) {
        let title = if it.icon_source.starts_with("steam:") || it.icon_source.starts_with("web:Game:") { clean_game_title(&it.file_name) } else { clean_title(&it.file_name) };
        if !title.is_empty() { return title; }
    }
    it.file_name.clone()
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

async fn best_match(term: &str, _q: &[String], title: &str) -> Option<(i64, String)> {
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
        let Some(score) = steam_score(title, name) else { continue };
        if score > best_score {
            best_score = score;
            best = Some((id, name.to_string()));
        }
    }
    best
}

/// The game's square icon, portrait cover and wide banner, all cached by verified app id.
async fn download_art(app: i64) -> ArtUrls {
    let _ = std::fs::create_dir_all(dir());
    (icon(app).await, cover(app).await, banner(app).await)
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

/// URL order is independent of matching: only a verified Steam app id reaches this path.
fn banner_urls(app: i64, data: &Value) -> Vec<String> {
    let mut urls = vec![format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{app}/library_hero.jpg")];
    urls.push(format!("https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/{app}/capsule_616x353.jpg"));
    if let Some(header) = data["header_image"].as_str().filter(|u| !u.trim().is_empty()) {
        urls.push(header.to_string());
    }
    urls
}

async fn banner(app: i64) -> Option<String> {
    let dest = dir().join(format!("steam-{app}-banner-v2.jpg"));
    if dest.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Some(dest.to_string_lossy().into_owned());
    }
    for url in banner_urls(app, &Value::Null) {
        if let Some(path) = save_image(&url, &dest).await { return Some(path); }
    }
    let doc = get_json(&format!("https://store.steampowered.com/api/appdetails?appids={app}&filters=basic")).await?;
    for url in banner_urls(app, &doc[app.to_string()]["data"]).iter().skip(2) {
        if let Some(path) = save_image(url, &dest).await { return Some(path); }
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
    banner: Option<String>,
    art_color: String,
}

fn artwork_due(it: &Item) -> bool {
    (it.icon_source.starts_with("steam:") || it.icon_source.starts_with("web:")
            || it.icon_source == "none" && worth_lookup(it) && it.icon_lookup_at.is_none_or(|t| t <= chrono::Local::now()))
        && (it.banner_revision < 16 || it.icon_lookup_at.is_none_or(|t| t < chrono::Local::now() - chrono::Duration::days(7)))
}

async fn repair_artwork(mgr: &Arc<Manager>, id: &str) {
    if !PENDING.lock().insert(id.to_string()) { return; }
    let _g = ONLINE.lock().await;
    if let Some(it) = mgr.item(id) {
        let mut existing = Found { art_color: it.art_color.clone(), source: it.icon_source.clone(), name: if it.steam_title.is_empty() { display_title(&it) } else { it.steam_title.clone() },
            // An icon FluxDM framed itself ("-square"/"-icon") is made again from the original, so a better framing rule applies.
            icon: (Path::new(&it.icon_file).is_file() && !it.icon_file.ends_with("-square.png") && !it.icon_file.ends_with("-icon.png")).then(|| it.icon_file.clone()), cover: Path::new(&it.cover_file).is_file().then(|| it.cover_file.clone()),
            banner: Path::new(&it.banner_file).is_file().then(|| it.banner_file.clone()) };
        if existing.banner.is_none() && existing.source.starts_with("web:") {
            let stem = format!("web-{}", &hex::encode(<sha1::Sha1 as sha1::Digest>::digest(existing.source.as_bytes()))[..16]);
            existing.banner = web_banner(&existing.source, &stem).await;
        }
        let found = enrich_art(&it, &display_title(&it), Some(existing)).await;
        let revision = 16;
        mgr.with_item(id, |x| {
            if x.icon_source != it.icon_source || x.file_name != it.file_name { return; }
            x.banner_revision = revision;
            x.icon_lookup_at = Some(chrono::Local::now());
            if let Some(f) = found {
                x.art_color = f.art_color;
                if let Some(p) = f.icon { x.icon_file = p; }
                if let Some(p) = f.cover { x.cover_file = p; }
                if let Some(p) = f.banner { if !is_user_banner(&x.banner_file) { x.banner_file = p; } }
            }
        });
        mgr.mark_dirty();
        if let Some(it) = mgr.item(id) { URLS.lock().insert(id.to_string(), compute(&it, &mgr.settings())); }
        mgr.push_list();
    }
    PENDING.lock().remove(id);
}

fn patch_host(id: &str) -> &'static str {
    if id.starts_with("CUSA") { "orbispatches.com" } else { "prosperopatches.com" }
}

/// Accept only official title artwork for this ID, never an unrelated page image.
fn patch_images(html: &str, id: &str) -> (Option<String>, Option<String>) {
    let pattern = format!(r#"https://cdn\.{}/titles/{}_[a-zA-Z0-9_-]+/(pic0|icon0)\.webp"#, regex::escape(patch_host(id)), regex::escape(id));
    let re = Regex::new(&pattern).unwrap();
    let html = html.replace("\\/", "/");
    let get = |kind: &str| re.captures_iter(&html).find(|c| &c[1] == kind).map(|c| c[0].to_string());
    (get("pic0"), get("icon0"))
}

async fn patch_art(id: &str) -> (Option<String>, Option<String>) {
    let result = async {
        let r = net::HTTP.get(format!("https://{}/{id}", patch_host(id))).header("User-Agent", UA).timeout(Duration::from_secs(20)).send().await.ok()?;
        if !r.status().is_success() || r.content_length().is_some_and(|n| n > 2_000_000) { return None; }
        let (banner, icon) = patch_images(&r.text().await.ok()?, id);
        let banner = if let Some(url) = banner { save_web(&url, &format!("web-ps-{id}-pic0")).await } else { None };
        let icon = if let Some(url) = icon { save_web(&url, &format!("web-ps-{id}-icon0")).await } else { None };
        Some((banner, icon))
    }.await;
    result.unwrap_or_default()
}

struct Vendor { wiki: &'static str, snap: &'static str, flat: &'static str, color: &'static str, pattern: &'static str, title: &'static str, page: &'static str }
const VENDORS: &[Vendor] = &[
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "steamsetup", title: "Steam", page: "https://store.steampowered.com/about/" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "nvidia", title: "NVIDIA app", page: "https://www.nvidia.com/en-us/software/nvidia-app/" },
    Vendor { wiki:"Discord", snap:"discord", flat:"com.discordapp.Discord", color:"", pattern: "discord", title: "Discord", page: "https://discord.com/download" },
    Vendor { wiki:"Blender_(software)", snap:"blender", flat:"org.blender.Blender", color:"", pattern: "blender", title: "Blender", page: "https://www.blender.org/download/" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "chrome", title: "Google Chrome", page: "https://www.google.com/chrome/" },
    Vendor { wiki:"Firefox", snap:"firefox", flat:"org.mozilla.firefox", color:"", pattern: "firefox", title: "Mozilla Firefox", page: "https://www.mozilla.org/firefox/" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "waterfox", title: "Waterfox", page: "https://www.waterfox.net/" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "epic", title: "Epic Games Launcher", page: "https://store.epicgames.com/download" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "eaapp|eadesktop", title: "EA app", page: "https://www.ea.com/ea-app" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "ubisoft", title: "Ubisoft Connect", page: "https://www.ubisoft.com/ubisoft-connect" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "battlenet|battle.net", title: "Battle.net", page: "https://download.battle.net/" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "galaxy", title: "GOG Galaxy", page: "https://www.gog.com/galaxy" },
    Vendor { wiki:"OBS_Studio", snap:"obs-studio", flat:"com.obsproject.Studio", color:"", pattern: "obs", title: "OBS Studio", page: "https://github.com/obsproject/obs-studio" },
    Vendor { wiki:"VLC_media_player", snap:"vlc", flat:"org.videolan.VLC", color:"", pattern: "vlc", title: "VLC", page: "https://github.com/videolan/vlc" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "python", title: "Python", page: "https://www.python.org/downloads/" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "vscode|vs_code|vs code", title: "Visual Studio Code", page: "https://code.visualstudio.com/download" },
    Vendor { wiki:"Spotify", snap:"spotify", flat:"com.spotify.Client", color:"", pattern: "spotify", title: "Spotify", page: "https://www.spotify.com/download/" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "whatsapp", title: "WhatsApp", page: "https://www.whatsapp.com/download" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "zoom", title: "Zoom", page: "https://zoom.us/download" },
    Vendor { wiki:"Telegram_(software)", snap:"telegram-desktop", flat:"org.telegram.desktop", color:"", pattern: "telegram", title: "Telegram", page: "https://desktop.telegram.org/" },
    Vendor { wiki:"QBittorrent", snap:"qbittorrent-arnat", flat:"org.qbittorrent.qBittorrent", color:"", pattern: "qbittorrent", title: "qBittorrent", page: "https://www.qbittorrent.org/download" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "afterburner", title: "MSI Afterburner", page: "https://www.msi.com/Landing/afterburner/graphics-cards" },
    Vendor { wiki:"Rufus_(software)", snap:"", flat:"", color:"", pattern: "rufus", title: "Rufus", page: "https://rufus.ie/" },
    Vendor { wiki:"Ventoy", snap:"", flat:"", color:"", pattern: "ventoy", title: "Ventoy", page: "https://www.ventoy.net/" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "windows|win10|win11", title: "Windows 11", page: "https://www.microsoft.com/en-us/windows/windows-11" },
    Vendor { wiki:"Ubuntu", snap:"", flat:"", color:"#E95420", pattern: "ubuntu", title: "Ubuntu", page: "https://ubuntu.com/download/desktop" },
    Vendor { wiki:"Fedora_Linux", snap:"", flat:"", color:"#51A2DA", pattern: "fedora", title: "Fedora", page: "https://fedoraproject.org/" },
    Vendor { wiki:"Linux_Mint", snap:"", flat:"", color:"#87CF3E", pattern: "linuxmint|linux mint", title: "Linux Mint", page: "https://linuxmint.com/download.php" },
    Vendor { wiki:"Debian", snap:"", flat:"", color:"#D70A53", pattern: "debian", title: "Debian", page: "https://www.debian.org/" },
    Vendor { wiki:"7-Zip", snap:"", flat:"", color:"", pattern: "7zip|7z", title: "7-Zip", page: "https://github.com/ip7z/7zip" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "node", title: "Node.js", page: "https://nodejs.org/en/download" },
    Vendor { wiki:"", snap:"", flat:"", color:"", pattern: "git", title: "Git", page: "https://git-scm.com/downloads" },
 ];
fn vendor(name: &str) -> Option<&'static Vendor> {
    let name = name.to_lowercase();
    VENDORS.iter().find(|v| v.pattern.split('|').any(|p| name.starts_with(p)))
}
fn program_item(it: &Item) -> bool {
    vendor(&it.file_name).is_some() || OS_EXT.contains(&ext_of(&it.file_name).as_str()) && !HAS_GAME_WORDS.is_match(&it.file_name)
        || matches!(ext_of(&it.file_name).as_str(), "exe" | "msi" | "msix" | "appx" | "deb" | "rpm" | "appimage")
}
fn github_repo(link: &str) -> Option<String> {
    let u = url::Url::parse(link).ok()?;
    if u.host_str()? != "github.com" { return None; }
    let parts: Vec<_> = u.path_segments()?.collect();
    if parts.len() < 5 || parts[2..4] != ["releases", "download"] { return None; }
    if parts[..2].iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))) { return None; }
    Some(format!("https://github.com/{}/{}",parts[0],parts[1]))
}
fn host_page(link: &str) -> Option<String> {
    let u = url::Url::parse(link).ok()?;
    if !matches!(u.scheme(),"https" | "http") { return None; }
    let mut host = u.host_str()?;
    while let Some(rest) = ["dl.","download.","cdn.","files."].iter().find_map(|p|host.strip_prefix(p)) { host = rest; }
    Some(format!("https://{host}/"))
}
fn referrer(it: &Item) -> Option<String> {
    it.headers.lines().find_map(|line| { let (k,v)=line.split_once(':')?; k.trim().eq_ignore_ascii_case("referer").then(||v.trim().to_string()) })
}
fn wide_image(w: u32, h: u32, min: u32) -> bool { w >= min && h > 0 && w as f64 / h as f64 >= 1.5 }
async fn page_banner(page: &str, kind: &str) -> Option<String> {
    web_banner(&format!("web:Software:{page}"), &format!("web-{kind}-{}",hash_stem(page))).await
}
fn wiki_image(doc: &Value) -> Option<String> {
    let i = &doc["originalimage"];
    wide_image(i["width"].as_u64()? as u32,i["height"].as_u64()? as u32,1000)
        .then(||i["source"].as_str().map(str::to_string)).flatten()
}
fn snap_images(doc: &Value, banner: bool) -> Vec<String> {
    doc["snap"]["media"].as_array().or_else(||doc["media"].as_array()).into_iter().flatten()
        .filter(|m|m["type"] == if banner { "banner" } else { "screenshot" })
        .filter_map(|m|m["url"].as_str().map(str::to_string)).collect()
}
fn flat_images(doc: &Value) -> Vec<String> {
    let mut images: Vec<_> = doc["screenshots"].as_array().into_iter().flatten()
        .flat_map(|s|s["sizes"].as_array().into_iter().flatten())
        .filter(|i|wide_image(i["width"].as_u64().unwrap_or(0) as u32,i["height"].as_u64().unwrap_or(0) as u32,800)).collect();
    images.sort_by_key(|i|std::cmp::Reverse(i["width"].as_u64().unwrap_or(0)));
    images.into_iter().filter_map(|i|i["url"].as_str().or(i["src"].as_str()).map(str::to_string)).collect()
}
fn valid_color(s: &str) -> bool { s.len()==7 && s.starts_with('#') && s[1..].bytes().all(|b|b.is_ascii_hexdigit()) }
fn flat_color(doc: &Value) -> Option<String> {
    doc["branding"].as_array().into_iter().flatten().filter_map(|b|b["color"].as_str())
        .chain(doc["branding"]["light"]["background"].as_str())
        .find(|c|valid_color(c)).map(str::to_string)
}
async fn save_wide(url: &str, kind: &str, min: u32) -> Option<String> {
    let p = save_web(url,&format!("web-{kind}-{}",hash_stem(url))).await?;
    if std::fs::read(&p).ok().and_then(|b|image_size(&b)).is_some_and(|(w,h)|wide_image(w,h,min)) { return Some(p); }
    let _ = std::fs::remove_file(p);
    None
}
async fn program_banner(it: &Item, title: &str) -> (Option<String>,String) {
    let v = vendor(&it.file_name);
    let mut color = v.map(|v|v.color).unwrap_or("").to_string();
    let title = v.map(|v|v.title.to_string()).unwrap_or_else(||clean_game_title(title));
    if let Some(p) = store_banner(&title,false).await { return (Some(p),color); }
    if let Some(v) = v { if let Some(p) = page_banner(v.page,"vendor").await { return (Some(p),color); } }
    if let Some(repo) = github_repo(&it.url) {
        if let Some(p) = page_banner(&repo,"repo").await { return (Some(p),color); }
    }
    for page in [referrer(it),host_page(&it.url)].into_iter().flatten() {
        if let Some(p) = page_banner(&page,"page").await { return (Some(p),color); }
    }
    let article = v.filter(|v|!v.wiki.is_empty()).map(|v|v.wiki.to_string()).or_else(||web_page(&it.icon_source).and_then(|p|p.split("/wiki/").nth(1).map(str::to_string)));
    if let Some(article) = article {
        if let Some(doc) = get_json(&format!("https://en.wikipedia.org/api/rest_v1/page/summary/{}",enc(&article))).await {
            if let Some(url) = wiki_image(&doc) { if let Some(p) = save_wide(&url,"wiki",1000).await { return (Some(p),color); } }
        }
    }
    let mut screenshots = vec![];
    if let Some(v) = v {
        if !v.snap.is_empty() {
            let response = net::HTTP.get(format!("https://api.snapcraft.io/v2/snaps/info/{}?fields=media",v.snap))
                .header("User-Agent",UA).header("Snap-Device-Series","16").timeout(Duration::from_secs(20)).send().await;
            if let Ok(r) = response { if r.status().is_success() { if let Ok(doc) = r.json::<Value>().await {
                for url in snap_images(&doc,true) { if let Some(p)=save_wide(&url,"snap",800).await { return (Some(p),color); } }
                screenshots.extend(snap_images(&doc,false));
            } } }
        }
        if !v.flat.is_empty() {
            if let Some(doc)=get_json(&format!("https://flathub.org/api/v2/appstream/{}",v.flat)).await {
                if let Some(c)=flat_color(&doc) { color=c; }
                screenshots.extend(flat_images(&doc));
            }
        }
    }
    for url in screenshots { if let Some(p)=save_wide(&url,"shot",800).await { return (Some(p),color); } }
    (None,color)
}

fn strict_title(a: &str, b: &str) -> bool {
    let norm = |s: &str| words(&tidy_name(s).to_lowercase().replace("(r)", "").replace("(tm)", "")).concat();
    !norm(a).is_empty() && norm(a) == norm(b)
}
fn store_match(doc: &Value, title: &str, game: bool) -> Option<String> {
    doc["Results"].as_array()?.iter().flat_map(|r| r["Products"].as_array().into_iter().flatten())
        .find(|p| (if game { p["Type"] == "Game" } else { matches!(p["Type"].as_str(),Some("App" | "Application")) }) && p["Title"].as_str().is_some_and(|n| strict_title(title,n)))?["ProductId"].as_str().map(str::to_string)
}
fn store_hero(doc: &Value) -> Option<String> {
    let image = doc["Products"][0]["LocalizedProperties"][0]["Images"].as_array()?.iter().find(|i| i["ImagePurpose"] == "SuperHeroArt")?;
    let uri = image["Uri"].as_str()?;
    if uri.starts_with("//") { Some(format!("https:{uri}")) } else { uri.starts_with("https://").then(|| uri.to_string()) }
}
async fn store_banner(title: &str, game: bool) -> Option<String> {
    let doc = get_json(&format!("https://displaycatalog.mp.microsoft.com/v7.0/productFamilies/autosuggest?market=US&languages=en-US&query={}&productFamilyNames=Apps,Games",enc(title))).await?;
    let id = store_match(&doc,title,game)?;
    let doc = get_json(&format!("https://displaycatalog.mp.microsoft.com/v7.0/products?bigIds={}&market=US&languages=en-US",enc(&id))).await?;
    save_web(&store_hero(&doc)?, &format!("web-ms-{}-hero",hash_stem(&id))).await
}
fn hash_stem(s: &str) -> String { hex::encode(<sha1::Sha1 as sha1::Digest>::digest(s.as_bytes()))[..16].to_string() }
fn ps_match(doc: &Value, title: &str) -> Option<String> {
    doc["results"].as_array()?.iter().find_map(|r| {
        let id = r["title_id"].as_str()?;
        (matches!(r["platform"].as_str(), Some("ps4" | "ps5")) && console_id(id).as_deref() == Some(id) && strict_title(title,r["name"].as_str()?)).then(|| id.to_string())
    })
}
/// GarlicSaves matches the query as one substring of the store name, and store names carry marks
/// mid-title ("God of War® III Remastered"), so shorter leading queries are tried; the match itself stays strict.
async fn ps_search(title: &str) -> Option<String> {
    for q in ps_queries(title) {
        if let Some(doc) = get_json(&format!("https://garlicsaves.com/tools/api/title-search?q={}", enc(&q))).await {
            if let Some(id) = ps_match(&doc, title) { return Some(id); }
        }
    }
    None
}
fn ps_queries(title: &str) -> Vec<String> {
    let w: Vec<&str> = title.split_whitespace().collect();
    let mut out = vec![w.join(" ")];
    for n in (2..w.len()).rev() {
        out.push(w[..n].join(" "));
        if out.len() == 3 { break; }
    }
    out.dedup();
    out
}
fn gog_image(doc: &Value, title: &str) -> Option<String> {
    doc["products"].as_array()?.iter().find(|p| p["title"].as_str().is_some_and(|n| strict_title(title,n)))?["coverHorizontal"].as_str().filter(|u| u.starts_with("https://")).map(str::to_string)
}
async fn gog_banner(title: &str) -> Option<String> {
    let doc = get_json(&format!("https://catalog.gog.com/v1/catalog?query=like:{}&limit=5",enc(title))).await?;
    save_web(&gog_image(&doc,title)?, &format!("web-gog-{}",hash_stem(title))).await
}

fn banner_rank(path: &str) -> u8 {
    let name = Path::new(path).file_name().unwrap_or_default().to_string_lossy();
    if name.ends_with("-hero-v3.jpg") { 0 }
    // Microsoft Store hero art leaves a wide empty area for the store's own text: only when nothing better exists.
    else if name.starts_with("web-ms-") { 5 }
    else if name.starts_with("web-ps-") && name.contains("-pic0.") { 2 }
    else if name.starts_with("web-gog-") { 3 }
    else if name.starts_with("steam-") { 4 }
    else if name.starts_with("web-vendor-") { 2 }
    else if name.starts_with("web-repo-") { 3 }
    else if name.starts_with("web-wiki-") { 8 }
    else if name.starts_with("web-snap-") { 9 }
    else if name.starts_with("web-shot-") { 10 }
    else { 7 }
}

fn preferred_banner(candidate: Option<String>, cached: Option<String>) -> Option<String> {
    match (candidate, cached) {
        (Some(a), Some(b)) => Some(if banner_rank(&a) <= banner_rank(&b) { a } else { b }),
        (a,b) => a.or(b),
    }
}

/// One priority order for persisted items and Add previews, independent of icon source.
async fn enrich_art(it: &Item, title: &str, found: Option<Found>) -> Option<Found> {
    let mut found = enrich_art_sources(it, title, found).await;
    if let Some(f) = found.as_mut() {
        // Games: a real square icon from the console/store pages comes first; the source's own icon and a framed
        // cover are only fallbacks.
        let game = f.source.starts_with("steam:") || f.source.starts_with("web:Game:");
        if game {
            let name = if f.name.trim().is_empty() { title } else { f.name.as_str() };
            if let Some(p) = game_icon(name).await { f.icon = Some(p); }
        }
        f.icon = sharp_icon(f.icon.take(), f.cover.as_deref());
        if let Some(p) = f.banner.take() {
            f.banner = tokio::task::spawn_blocking(move || fit_banner(&p, true)).await.ok().flatten();
        }
    }
    found
}

/// A square picture of at least 256 px - good enough to show as is.
fn square_and_sharp(path: Option<&str>) -> bool {
    path.and_then(|p| std::fs::read(p).ok()).and_then(|b| image_size(&b))
        .is_some_and(|(w, h)| w.min(h) >= 256 && (w.max(h) as f32) <= w.min(h) as f32 * 1.08)
}

/// Large square game icons, best first: the PlayStation store icon (512 px, found by name), then the Microsoft
/// Store's square box art. Only when neither exists is an icon framed from a cover.
async fn game_icon(title: &str) -> Option<String> {
    if let Some(id) = ps_search(title).await {
        if let (_, Some(icon)) = patch_art(&id).await {
            if square_and_sharp(Some(&icon)) { return Some(icon); }
        }
    }
    let doc = get_json(&format!("https://displaycatalog.mp.microsoft.com/v7.0/productFamilies/autosuggest?market=US&languages=en-US&query={}&productFamilyNames=Games", enc(title))).await?;
    let id = store_match(&doc, title, true)?;
    let doc = get_json(&format!("https://displaycatalog.mp.microsoft.com/v7.0/products?bigIds={}&market=US&languages=en-US", enc(&id))).await?;
    let url = store_square(&doc)?;
    let p = save_web(&url, &format!("web-ms-{}-square", hash_stem(&id))).await?;
    square_and_sharp(Some(&p)).then_some(p)
}

/// The Microsoft Store's largest square artwork (box art first, then tiles and logos).
fn store_square(doc: &Value) -> Option<String> {
    let images = doc["Products"][0]["LocalizedProperties"][0]["Images"].as_array()?;
    let rank = |purpose: &str| match purpose { "BoxArt" => 0, "FeaturePromotionalSquareArt" => 1, "Tile" => 2, "Logo" => 3, _ => 9 };
    images.iter()
        .filter(|i| i["Width"].as_i64().is_some_and(|w| w >= 300 && Some(w) == i["Height"].as_i64()) && rank(i["ImagePurpose"].as_str().unwrap_or("")) < 9)
        .min_by_key(|i| (rank(i["ImagePurpose"].as_str().unwrap_or("")), -i["Width"].as_i64().unwrap_or(0)))
        .and_then(|i| i["Uri"].as_str())
        .map(|u| if u.starts_with("//") { format!("https:{u}") } else { u.to_string() })
}
/// The icon to show: a tiny one (Steam's own icons are 32 px) looks blurry when shown at 72 px, so the
/// cover is used instead when the icon is under 128 px and the cover is bigger (the UI crops it square).
fn sharp_icon(icon: Option<String>, cover: Option<&str>) -> Option<String> {
    let side = |p: &str| std::fs::read(p).ok().and_then(|b| image_size(&b)).map(|(w, h)| w.min(h)).unwrap_or(0);
    let chosen = match (icon, cover) {
        (Some(i), Some(c)) if side(&i) < 128 && side(c) > side(&i) => c.to_string(),
        (Some(i), _) => i,
        (None, c) => c?.to_string(),
    };
    Some(square_icon(&chosen).unwrap_or(chosen))
}

/// A wide or tall picture used as an icon is framed around its detail - title lettering and faces have far more
/// fine detail than sky, walls or blur - instead of the plain middle, which cut the title or the face off. When
/// the detail (say a face on one side and the logo on the other) is wider than a square, the frame zooms out to
/// hold all of it and the bands above and below are filled with a blurred copy of the same picture, like app
/// store icons. Saved once as "<name>-icon.png" (max 512 px); square pictures are kept as they are.
fn square_icon(path: &str) -> Option<String> {
    let p = Path::new(path);
    let out = p.with_file_name(format!("{}-icon.png", p.file_stem()?.to_string_lossy()));
    if out.is_file() { return Some(out.to_string_lossy().into_owned()); }
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w.max(h) as f32 <= w.min(h) as f32 * 1.08 { return None; }
    let (wide, side, long) = (w > h, w.min(h), w.max(h));
    // Detail along the long side, measured on a small copy: how much neighbouring pixels differ.
    let scale = 192.0 / long as f32;
    let small = image::imageops::resize(&img, ((w as f32 * scale).max(1.0)) as u32, ((h as f32 * scale).max(1.0)) as u32, image::imageops::FilterType::Triangle);
    let (sw, sh) = small.dimensions();
    let luma = |x: u32, y: u32| { let p = small.get_pixel(x, y); 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32 };
    let mut energy = vec![0f32; if wide { sw } else { sh } as usize];
    for y in 0..sh.saturating_sub(1) {
        for x in 0..sw.saturating_sub(1) {
            let e = (luma(x, y) - luma(x + 1, y)).abs() + (luma(x, y) - luma(x, y + 1)).abs();
            energy[if wide { x } else { y } as usize] += e;
        }
    }
    let n = energy.len();
    let total: f32 = energy.iter().sum::<f32>().max(1.0);
    // Narrowest span holding 75% of the detail, then kept between one square and 1.8 squares long (a whole 16:9 cover).
    let min_win = ((side as f32 * scale).round() as usize).clamp(1, n);
    let max_win = ((side as f32 * 1.8 * scale).round() as usize).clamp(min_win, n);
    let mut span = (0usize, n);
    let (mut lo, mut sum) = (0usize, 0f32);
    for hi in 0..n {
        sum += energy[hi];
        while lo < hi && sum - energy[lo] >= total * 0.75 { sum -= energy[lo]; lo += 1; }
        if sum >= total * 0.75 && hi + 1 - lo < span.1 - span.0 { span = (lo, hi + 1); }
    }
    let win = (span.1 - span.0).clamp(min_win, max_win);
    let centre = (span.0 + span.1) as f32 / 2.0;
    let start = ((centre - win as f32 / 2.0).round().max(0.0) as usize).min(n - win);
    let (from, len) = (((start as f32 / scale).round() as u32).min(long - side), ((win as f32 / scale).round() as u32).clamp(side, long));
    let from = from.min(long - len);
    let size = side.min(512);
    let region = if wide { image::imageops::crop_imm(&img, from, 0, len, side).to_image() } else { image::imageops::crop_imm(&img, 0, from, side, len).to_image() };
    let icon = if len <= side + side / 20 {
        image::imageops::resize(&region, size, size, image::imageops::FilterType::Lanczos3)
    } else {
        // Blurred, slightly darker copy filling the square behind the whole detailed span.
        let fill = image::imageops::resize(&region, size, size, image::imageops::FilterType::Triangle);
        let mut back = image::imageops::blur(&fill, size as f32 / 18.0);
        for px in back.pixels_mut() { for c in 0..3 { px[c] = (px[c] as f32 * 0.7) as u8; } }
        let (fw, fh) = if wide { (size, (size as f32 * side as f32 / len as f32).round() as u32) } else { ((size as f32 * side as f32 / len as f32).round() as u32, size) };
        let front = image::imageops::resize(&region, fw.max(1), fh.max(1), image::imageops::FilterType::Lanczos3);
        image::imageops::overlay(&mut back, &front, ((size - fw.min(size)) / 2) as i64, ((size - fh.min(size)) / 2) as i64);
        back
    };
    icon.save(&out).ok()?;
    Some(out.to_string_lossy().into_owned())
}
async fn enrich_art_sources(it: &Item, title: &str, mut found: Option<Found>) -> Option<Found> {
    let game = found.as_ref().is_some_and(|f| f.source.starts_with("steam:") || f.source.starts_with("web:Game:"))
        || found.is_none() && !program_item(it) && plan(it, &it.file_name, None).contains(&Src::Game);
    if !game {
        if program_item(it) || found.as_ref().is_some_and(|f| f.source.starts_with("web:Software:") || f.source.starts_with("web:Os:") || f.source.starts_with("web:Emulator:")) {
            let _ = std::fs::create_dir_all(dir());
            let (banner,color) = program_banner(it,title).await;
            if banner.is_some() || !color.is_empty() {
                let f = found.get_or_insert_with(||Found { art_color: String::new(), source:"web:Software:artwork".into(),name:title.to_string(),icon:None,cover:None,banner:None });
                f.banner = preferred_banner(banner,f.banner.take());
                if !color.is_empty() { f.art_color = color; }
            }
        }
        return found;
    }
    let _ = std::fs::create_dir_all(dir());
    let lookup_title = found.as_ref().filter(|f| f.source.starts_with("steam:")).map(|f| f.name.clone()).unwrap_or_else(|| clean_game_title(title));
    let app = found.as_ref().and_then(|f| f.source.strip_prefix("steam:")).and_then(|a| a.parse::<i64>().ok());
    let mut hero = None;
    if let Some(app) = app {
        let dest = dir().join(format!("steam-{app}-hero-v3.jpg"));
        hero = if dest.is_file() { Some(dest.to_string_lossy().into_owned()) }
            else { save_image(&banner_urls(app, &Value::Null)[0], &dest).await };
    }
    if hero.is_none() { hero = store_banner(&lookup_title, true).await; }
    let ps_id = if let Some(id) = console_id(&it.file_name) { Some(id) } else if hero.is_none() { ps_search(&lookup_title).await } else { None };
    let (ps_banner, ps_icon) = if let Some(id) = ps_id { patch_art(&id).await } else { (None,None) };
    if hero.is_none() { hero = ps_banner; }
    if hero.is_none() { hero = gog_banner(&lookup_title).await; }
    if found.is_none() && (hero.is_some() || ps_icon.is_some()) {
        found = Some(Found { art_color: String::new(), source: "web:Game:artwork".into(), name: lookup_title, icon: None, cover: None, banner: None });
    }
    if let Some(f) = &mut found {
        if let Some(app) = app {
            if f.banner.is_none() && hero.is_none() { f.banner = banner(app).await; }
            if f.icon.is_none() { f.icon = icon(app).await; }
            if f.cover.is_none() { f.cover = cover(app).await; }
        }
        if f.icon.is_none() { f.icon = ps_icon; }
        f.banner = preferred_banner(hero, f.banner.take());
    }
    found
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
// Separators like "_" and "-" count as word breaks ("DLCs_PS5", "Game-PS4").
static HAS_GAME_WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|[^a-z0-9])(ps5|ps4|ps3|ps2|psp|vita|switch|xbox|wii|nds|3ds|gamecube|rom)(?:$|[^a-z0-9])|(?:^|[^a-z0-9])(?:ppsa|cusa)\d{5}").unwrap());
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
    let found = search_source(src, title).await?;
    // A soundtrack, demo, artbook… shares the name but isn't the thing being downloaded, unless the name says so.
    (src == Src::Album || !SIDE.is_match(&found.name) || SIDE.is_match(title)).then_some(found)
}

async fn search_source(src: Src, title: &str) -> Option<Found> {
    // Media searches omit release tags without changing the item's displayed title.
    let media_query;
    let title = if matches!(src, Src::Movie | Src::Tv | Src::Anime | Src::Album) {
        media_query = clean_game_title(title);
        media_query.as_str()
    } else { title };
    match src {
        Src::Steam => {
            let (app, name) = find(&clean_game_title(title)).await?;
            let (icon, cover, banner) = download_art(app).await;
            (icon.is_some() || cover.is_some() || banner.is_some()).then(|| Found { art_color: String::new(), source: format!("steam:{app}"), name, icon, cover, banner })
        }
        // Each kind tries several free sources in turn; the ones with wide backdrops come first.
        Src::Tv => {
            if let Some((name, img, page, wide)) = tvmaze(title).await {
                if let Some(f) = web_found_wide(src, name, &img, &page, wide).await { return Some(f); }
            }
            if let Some((name, img, page, wide)) = cinemeta(title, "series").await {
                if let Some(f) = web_found_wide(src, name, &img, &page, wide).await { return Some(f); }
            }
            if let Some((name, img, page)) = wiki(title, src).await {
                if let Some(f) = web_found(src, name, &img, &page).await { return Some(f); }
            }
            let (name, img, page) = itunes(title, src).await?;
            web_found(src, name, &img, &page).await
        }
        Src::Movie => {
            if let Some((name, img, page, wide)) = cinemeta(title, "movie").await {
                if let Some(f) = web_found_wide(src, name, &img, &page, wide).await { return Some(f); }
            }
            if let Some((name, img, page)) = wiki(title, src).await {
                if let Some(f) = web_found(src, name, &img, &page).await { return Some(f); }
            }
            let (name, img, page) = itunes(title, src).await?;
            web_found(src, name, &img, &page).await
        }
        Src::Game | Src::Software | Src::Emulator | Src::Os => {
            let (name, img, page) = wiki(title, src).await?;
            web_found(src, name, &img, &page).await
        }
        Src::Book => {
            let (name, img, page) = open_library(title).await?;
            web_found(src, name, &img, &page).await
        }
        Src::Album => {
            let (name, img, page) = itunes(title, src).await?;
            web_found(src, name, &img, &page).await
        }
        Src::Anime => {
            if let Some((name, img, page, wide)) = anilist(title).await {
                if let Some(f) = web_found_wide(src, name, &img, &page, wide).await { return Some(f); }
            }
            if let Some((name, img, page)) = jikan(title).await {
                if let Some(f) = web_found(src, name, &img, &page).await { return Some(f); }
            }
            let (name, img, page, wide) = kitsu(title).await?;
            web_found_wide(src, name, &img, &page, wide).await
        }
    }
}

async fn web_found(src: Src, name: String, img: &str, page: &str) -> Option<Found> {
    web_found_wide(src, name, img, page, None).await
}

/// Like web_found, with the wide picture (backdrop/banner) the source itself offers; the page's share image is
/// only the fallback.
async fn web_found_wide(src: Src, name: String, img: &str, page: &str, wide: Option<String>) -> Option<Found> {
    let _ = std::fs::create_dir_all(dir());
    let key = format!("{src:?}:{page}");
    let stem = format!("web-{}", &hex::encode(<sha1::Sha1 as sha1::Digest>::digest(key.as_bytes()))[..16]);
    let path = save_web(img, &stem).await?;
    let source = format!("web:{src:?}:{page}");
    let banner = match wide {
        Some(u) => match save_web(&u, &format!("{stem}-wide")).await { Some(p) => Some(p), None => web_banner(&source, &stem).await },
        None => web_banner(&source, &stem).await,
    };
    Some(Found { art_color: String::new(), source, name, icon: Some(path.clone()), cover: Some(path), banner })
}


fn web_page(source: &str) -> Option<String> {
    let rest = source.strip_prefix("web:")?;
    if rest.starts_with("https://") || rest.starts_with("http://") { return Some(rest.to_string()); }
    let (kind, name) = rest.split_once(':')?;
    if name.starts_with("https://") || name.starts_with("http://") { return Some(name.to_string()); }
    matches!(kind, "Game" | "Software" | "Emulator" | "Os" | "Movie" | "Tv").then(|| format!("https://en.wikipedia.org/wiki/{}", enc(&name.replace(' ', "_"))))
}

fn social_images(html: &str, page: &str) -> Vec<String> {
    static META: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<meta\s+[^>]*>").unwrap());
    static ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)([\w:-]+)\s*=\s*["']([^"']*)["']"#).unwrap());
    let Ok(base) = url::Url::parse(page) else { return vec![] };
    META.find_iter(html).filter_map(|tag| {
        let attrs: HashMap<_, _> = ATTR.captures_iter(tag.as_str()).map(|c| (c[1].to_lowercase(), c[2].to_string())).collect();
        let key = attrs.get("property").or_else(|| attrs.get("name"))?;
        if !["og:image", "twitter:image", "twitter:image:src"].contains(&key.to_lowercase().as_str()) { return None; }
        let u = base.join(&attrs.get("content")?.replace("&amp;", "&")).ok()?;
        matches!(u.scheme(), "http" | "https").then(|| u.to_string())
    }).collect()
}

/// Read dimensions without decoding the downloaded image into a full bitmap.
fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be = |s: &[u8]| -> Option<u32> { Some(u32::from_be_bytes(s.try_into().ok()?)) };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") && b.len() >= 24 { return Some((be(&b[16..20])?, be(&b[20..24])?)); }
    if b.starts_with(b"GIF8") && b.len() >= 10 { return Some((u16::from_le_bytes(b[6..8].try_into().ok()?) as u32, u16::from_le_bytes(b[8..10].try_into().ok()?) as u32)); }
    if b.starts_with(&[255,216]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 255 { return None; }
            let marker = b[i+1]; i += 2;
            if marker == 255 { i -= 1; continue; }
            if matches!(marker, 0xD8 | 0xD9 | 0x01 | 0xD0..=0xD7) { continue; }
            let len = u16::from_be_bytes(b.get(i..i+2)?.try_into().ok()?) as usize;
            if len < 2 || i + len > b.len() { return None; }
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) && len >= 7 {
                return Some((u16::from_be_bytes(b[i+5..i+7].try_into().ok()?) as u32, u16::from_be_bytes(b[i+3..i+5].try_into().ok()?) as u32));
            }
            i += len;
        }
    }
    if b.len() >= 30 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        let le24 = |s: &[u8]| s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16;
        if &b[12..16] == b"VP8L" && b[20] == 0x2f {
            let n = u32::from_le_bytes(b[21..25].try_into().ok()?);
            return Some((1 + (n & 0x3fff), 1 + ((n >> 14) & 0x3fff)));
        }
        if &b[12..16] == b"VP8X" { return Some((1 + le24(&b[24..27]), 1 + le24(&b[27..30]))); }
        if &b[12..16] == b"VP8 " && &b[23..26] == b"\x9d\x01\x2a" { return Some(((u16::from_le_bytes(b[26..28].try_into().ok()?) & 0x3fff) as u32, (u16::from_le_bytes(b[28..30].try_into().ok()?) & 0x3fff) as u32)); }
    }
    None
}

/// The picture's content without blank padding: a band on two opposite sides (top+bottom or left+right) whose
/// pixels all match the corner colour - like a square album cover on a white 1200x630 share image. Natural
/// one-sided areas (a dark sky at the top) are not padding and are kept.
fn content_box(img: &image::RgbaImage) -> (u32, u32, u32, u32) {
    let (w, h) = img.dimensions();
    let base = *img.get_pixel(0, 0);
    let same = |p: &image::Rgba<u8>| p[3] < 16 || (0..3).all(|c| (p[c] as i32 - base[c] as i32).abs() <= 18);
    let row_blank = |y: u32| (0..w).filter(|&x| same(img.get_pixel(x, y))).count() as f32 >= w as f32 * 0.97;
    let col_blank = |x: u32| (0..h).filter(|&y| same(img.get_pixel(x, y))).count() as f32 >= h as f32 * 0.97;
    let (mut top, mut bottom, mut left, mut right) = (0, h, 0, w);
    while top < h / 2 && row_blank(top) { top += 1; }
    while bottom > h / 2 && row_blank(bottom - 1) { bottom -= 1; }
    while left < w / 2 && col_blank(left) { left += 1; }
    while right > w / 2 && col_blank(right - 1) { right -= 1; }
    let band = |a: u32, b: u32, size: u32| a >= size / 50 && b >= size / 50;
    let (y0, y1) = if band(top, h - bottom, h) { (top, bottom) } else { (0, h) };
    let (x0, x1) = if band(left, w - right, w) { (left, right) } else { (0, w) };
    (x0, y0, x1, y1)
}

/// Every banner passes this before it's used, whatever source it came from: blank padding is cropped off, and
/// with `strict` a picture that isn't wide and big enough afterwards is turned down, so the header shows the
/// blurred icon/cover instead of a boxed, square or tiny picture. Returns the path to use (a "-fit.png" copy
/// when something was cropped).
pub fn fit_banner(path: &str, strict: bool) -> Option<String> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let (x0, y0, x1, y1) = content_box(&img);
    let (cw, ch) = (x1 - x0, y1 - y0);
    if strict && (cw < 640 || (cw as f32) < ch as f32 * 1.45) {
        return None;
    }
    if (cw, ch) == (w, h) {
        return Some(path.to_string());
    }
    let p = Path::new(path);
    let out = p.with_file_name(format!("{}-fit.png", p.file_stem()?.to_string_lossy()));
    if !out.is_file() {
        image::imageops::crop_imm(&img, x0, y0, cw, ch).to_image().save(&out).ok()?;
    }
    Some(out.to_string_lossy().into_owned())
}
async fn web_banner(source: &str, stem: &str) -> Option<String> {
    let page = web_page(source)?;
    let r = net::HTTP.get(&page).header("User-Agent", UA).timeout(Duration::from_secs(20)).send().await.ok()?;
    if !r.status().is_success() || r.content_length().is_some_and(|n| n > 2_000_000) { return None; }
    let page = r.url().to_string();
    let html = r.text().await.ok()?;
    for u in social_images(&html, &page) {
        if let Some(p) = save_web(&u, &format!("{stem}-banner")).await {
            let wide = std::fs::read(&p).ok().and_then(|b| image_size(&b)).is_some_and(|(w,h)| wide_image(w,h,800));
            if wide { return Some(p); }
            let _ = std::fs::remove_file(&p);
        }
    }
    None
}

async fn repair_web_banner(mgr: &Arc<Manager>, id: &str) {
    repair_artwork(mgr, id).await;
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
    } else if ct.is_empty() || ct.contains("octet-stream") {
        "" // some CDNs (the PlayStation patch sites) send no type: decided from the bytes below
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
    let ext = if ext.is_empty() { sniff_image(&b)? } else { ext };
    let dest = dir().join(format!("{stem}.{ext}"));
    std::fs::write(&dest, &b).ok()?;
    Some(dest.to_string_lossy().into_owned())
}

/// The image type from the file's first bytes, for servers that don't send a content type.
fn sniff_image(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG") { Some("png") }
    else if b.starts_with(&[0xFF, 0xD8, 0xFF]) { Some("jpg") }
    else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" { Some("webp") }
    else if b.starts_with(b"GIF8") { Some("gif") }
    else { None }
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
async fn tvmaze(title: &str) -> Option<(String, String, String, Option<String>)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let doc = get_json(&format!("https://api.tvmaze.com/search/shows?q={}", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, String, String, i64)> = None;
    for e in doc.as_array()?.iter().take(6) {
        let (Some(name), Some(img)) = (e["show"]["name"].as_str(), e["show"]["image"]["original"].as_str().or(e["show"]["image"]["medium"].as_str())) else { continue };
        let Some(n) = name_matches(&q, name, 0.5) else { continue };
        if best.as_ref().is_none_or(|b| n > b.0) {
            best = Some((n, name.to_string(), img.replace("http://", "https://"), e["show"]["url"].as_str().unwrap_or("").to_string(), e["show"]["id"].as_i64().unwrap_or(0)));
        }
    }
    let (_, name, img, page, id) = best?;
    // The show's wide background picture, when it has one (largest first).
    let wide = if id > 0 {
        get_json(&format!("https://api.tvmaze.com/shows/{id}/images")).await.and_then(|v| {
            v.as_array()?.iter().filter(|i| i["type"] == "background")
                .filter_map(|i| i["resolutions"]["original"]["url"].as_str().map(|u| (i["resolutions"]["original"]["width"].as_i64().unwrap_or(0), u.replace("http://", "https://"))))
                .max_by_key(|(w, _)| *w).map(|(_, u)| u)
        })
    } else { None };
    Some((name, img, page, wide))
}

/// Cinemeta (the free catalogue behind Stremio): posters and wide 1920x1080 backgrounds for films ("movie")
/// and TV series ("series"), no key needed.
async fn cinemeta(title: &str, kind: &str) -> Option<(String, String, String, Option<String>)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let doc = get_json(&format!("https://v3-cinemeta.strem.io/catalog/{kind}/top/search={}.json", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, String, String, Option<String>)> = None;
    for m in doc["metas"].as_array()?.iter().take(8) {
        let (Some(name), Some(poster)) = (m["name"].as_str(), m["poster"].as_str()) else { continue };
        let Some(n) = name_matches(&q, name, 0.5) else { continue };
        if best.as_ref().is_none_or(|b| n > b.0) {
            let page = m["id"].as_str().filter(|i| i.starts_with("tt")).map(|i| format!("https://www.imdb.com/title/{i}/")).unwrap_or_default();
            let wide = m["background"].as_str().filter(|u| u.starts_with("https://")).map(|u| u.replace("/medium/", "/large/"));
            best = Some((n, name.to_string(), poster.to_string(), page, wide));
        }
    }
    best.map(|(_, n, i, page, wide)| (n, i, page, wide))
}

/// AniList: anime posters and wide banners, no key needed.
async fn anilist(title: &str) -> Option<(String, String, String, Option<String>)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let body = serde_json::json!({
        "query": "query($s:String){Page(perPage:6){media(search:$s,type:ANIME){title{romaji english} coverImage{extraLarge large} bannerImage siteUrl}}}",
        "variables": { "s": q.join(" ") },
    });
    let r = net::HTTP.post("https://graphql.anilist.co").header("User-Agent", UA).json(&body).timeout(Duration::from_secs(20)).send().await;
    let doc: Value = match r {
        Ok(r) if r.status().is_success() => r.json().await.ok()?,
        Ok(_) | Err(_) => { NET_FAILS.fetch_add(1, Ordering::Relaxed); return None; }
    };
    let mut best: Option<(usize, String, String, String, Option<String>)> = None;
    for m in doc["data"]["Page"]["media"].as_array()? {
        let Some(img) = m["coverImage"]["extraLarge"].as_str().or(m["coverImage"]["large"].as_str()) else { continue };
        for key in ["english", "romaji"] {
            let Some(name) = m["title"][key].as_str() else { continue };
            if let Some(n) = name_matches(&q, name, 0.5) {
                if best.as_ref().is_none_or(|b| n > b.0) {
                    best = Some((n, name.to_string(), img.to_string(), m["siteUrl"].as_str().unwrap_or("").to_string(), m["bannerImage"].as_str().map(str::to_string)));
                }
            }
        }
    }
    best.map(|(_, n, i, page, wide)| (n, i, page, wide))
}

/// Kitsu: another free anime catalogue (posters and wide cover pictures), the last anime fallback.
async fn kitsu(title: &str) -> Option<(String, String, String, Option<String>)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let doc = get_json(&format!("https://kitsu.io/api/edge/anime?filter%5Btext%5D={}&page%5Blimit%5D=6", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, String, String, Option<String>)> = None;
    for a in doc["data"].as_array()? {
        let at = &a["attributes"];
        let Some(img) = at["posterImage"]["large"].as_str().or(at["posterImage"]["original"].as_str()) else { continue };
        for name in [at["canonicalTitle"].as_str(), at["titles"]["en"].as_str(), at["titles"]["en_jp"].as_str()].into_iter().flatten() {
            if let Some(n) = name_matches(&q, name, 0.5) {
                if best.as_ref().is_none_or(|b| n > b.0) {
                    let page = at["slug"].as_str().map(|s| format!("https://kitsu.io/anime/{s}")).unwrap_or_default();
                    best = Some((n, name.to_string(), img.to_string(), page, at["coverImage"]["original"].as_str().map(str::to_string)));
                }
            }
        }
    }
    best.map(|(_, n, i, page, wide)| (n, i, page, wide))
}

/// Wikipedia: logos of software, operating systems and emulators, box art of console games.
async fn wiki(title: &str, kind: Src) -> Option<(String, String, String)> {
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
            best = Some((same, n, rank, name.to_string(), img.to_string()));
        }
    }
    best.map(|b| (strip_parens(&b.3), b.4, format!("https://en.wikipedia.org/wiki/{}", enc(&b.3.replace(' ', "_")))))
}

/// Open Library: book covers by title (the file name usually carries the author too).
async fn open_library(title: &str) -> Option<(String, String, String)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let doc = get_json(&format!("https://openlibrary.org/search.json?q={}&limit=8&fields=title,cover_i,key", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, i64, String)> = None;
    for d in doc["docs"].as_array()? {
        let (Some(name), Some(cover)) = (d["title"].as_str(), d["cover_i"].as_i64()) else { continue };
        let Some(n) = name_matches(&q, name, 0.3) else { continue };
        if n < 2 {
            continue; // a one-word title is too easy to hit by accident
        }
        if best.as_ref().is_none_or(|b| n > b.0) {
            best = Some((n, name.to_string(), cover, format!("https://openlibrary.org{}", d["key"].as_str().unwrap_or(""))));
        }
    }
    best.map(|(_, name, cover, page)| (name, format!("https://covers.openlibrary.org/b/id/{cover}-L.jpg"), page))
}

/// iTunes search: movie posters, TV season art, album covers.
async fn itunes(title: &str, kind: Src) -> Option<(String, String, String)> {
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
    let mut best: Option<(usize, String, String, String)> = None;
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
            best = Some((n, name, img.replace("100x100bb", "600x600bb"), r["collectionViewUrl"].as_str().or(r["trackViewUrl"].as_str()).unwrap_or("").to_string()));
        }
    }
    best.map(|(_, n, i, page)| (n, i, page))
}

/// Jikan (MyAnimeList): anime posters.
async fn jikan(title: &str) -> Option<(String, String, String)> {
    let q = plain(title, true);
    if q.is_empty() {
        return None;
    }
    let doc = get_json(&format!("https://api.jikan.moe/v4/anime?q={}&limit=6&sfw=false", enc(&q.join(" ")))).await?;
    let mut best: Option<(usize, String, String, String)> = None;
    for a in doc["data"].as_array()? {
        let Some(img) = a["images"]["jpg"]["large_image_url"].as_str() else { continue };
        for key in ["title", "title_english"] {
            let Some(name) = a[key].as_str() else { continue };
            if let Some(n) = name_matches(&q, name, 0.5) {
                if best.as_ref().is_none_or(|b| n > b.0) {
                    best = Some((n, name.to_string(), img.to_string(), a["url"].as_str().unwrap_or("").to_string()));
                }
            }
        }
    }
    best.map(|(_, n, i, page)| (n, i, page))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_migration_retries_old_console_misses_but_preserves_file_choice() {
        let mut it = item("PPSA21567.pkg", false);
        it.icon_source = "none".into();
        it.icon_lookup_at = Some(chrono::Local::now());
        assert!(artwork_due(&it));
        it.banner_revision = 9;
        assert!(artwork_due(&it), "revision 16 re-checks every banner and icon once");
        it.banner_revision = 16;
        assert!(!artwork_due(&it));
        it.banner_revision = 0;
        it.icon_lookup_at = Some(chrono::Local::now() + chrono::Duration::days(3650));
        assert!(!artwork_due(&it));
        it.icon_source = "user".into();
        assert!(!artwork_due(&it));
    }

    #[test]
    fn banner_priority_preserves_better_cached_art_during_failures() {
        let paths = ["steam-42-hero-v3.jpg", "web-ps-CUSA00362-pic0.webp", "web-gog-42.jpg", "steam-42-banner-v2.jpg", "web-ms-42-hero.webp", "web-page-banner.jpg", "web-shot-42.jpg"];
        for (a, candidate) in paths.iter().enumerate() {
            for (b, cached) in paths.iter().enumerate() {
                assert_eq!(preferred_banner(Some((*candidate).into()), Some((*cached).into())).as_deref(), Some(paths[a.min(b)]));
            }
            assert_eq!(preferred_banner(None, Some((*candidate).into())).as_deref(), Some(*candidate));
        }
        assert_eq!(preferred_banner(None, None), None);
    }

    #[test]
    fn webp_art_uses_its_content_type_and_dimensions() {
        assert_eq!(image_mime("pic0.webp"), "image/webp");
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff_image(b"<html>"), None);
        assert_eq!(ps_queries("God of War III Remastered"), ["God of War III Remastered", "God of War III", "God of War"]);
        assert_eq!(ps_queries("Bloodborne"), ["Bloodborne"]);
        let doc = serde_json::json!({"results":[{"name":"God of War\u{00ae} III Remastered","platform":"ps4","title_id":"CUSA01623"}]});
        assert_eq!(ps_match(&doc, "God of War III Remastered").as_deref(), Some("CUSA01623"));
        assert_eq!(image_mime("ICON0.WEBP"), "image/webp");
        assert_eq!(image_mime("hero.jpg"), "image/jpeg");
        let mut b = vec![0;30];
        b[..4].copy_from_slice(b"RIFF"); b[8..12].copy_from_slice(b"WEBP"); b[12..16].copy_from_slice(b"VP8X");
        b[24..27].copy_from_slice(&[0x7f,0x07,0]); b[27..30].copy_from_slice(&[0x37,0x04,0]);
        assert_eq!(image_size(&b), Some((1920,1080)));
    }


    #[test]
    fn saved_patch_pages_supply_only_matching_official_art() {
        for (html, id, host) in [
            (include_str!("../../tests/fixtures/artwork/orbis.html"), "CUSA00362", "orbispatches.com"),
            (include_str!("../../tests/fixtures/artwork/prospero.html"), "PPSA21567", "prosperopatches.com"),
        ] {
            assert!(id_title_from_html(html, id).is_some());
            let (banner, icon) = patch_images(html, id);
            assert!(banner.unwrap().starts_with(&format!("https://cdn.{host}/titles/{id}_")));
            assert!(icon.unwrap().ends_with("/icon0.webp"));
            assert_eq!(patch_images(html, "CUSA00000"), (None, None));
        }
        assert_eq!(patch_images("<title>Not found</title>", "PPSA21567"), (None,None));
    }

    #[test]
    fn last_resort_fixtures_reject_logos_and_select_wide_screenshots() {
        let doc:Value=serde_json::from_str(include_str!("../../tests/fixtures/artwork/program-fallbacks.json")).unwrap();
        assert_eq!(wiki_image(&doc["wiki"]).as_deref(),Some("https://example.invalid/vlc.jpg"));
        assert_eq!(wiki_image(&doc["logo"]),None);
        assert_eq!(snap_images(&doc["snap"],true),vec!["https://example.invalid/banner.png"]);
        assert_eq!(snap_images(&doc["snap"],false),vec!["https://example.invalid/screen.png"]);
        assert_eq!(flat_images(&doc["flat"]),vec!["https://example.invalid/screen.png"]);
        assert_eq!(flat_color(&doc["flat"]).as_deref(),Some("#123456"));
        assert!(!valid_color("red;display:none"));
        for name in ["ubuntu.iso","linuxmint.iso","fedora.iso"] { assert!(valid_color(vendor(name).unwrap().color)); }
        assert!(vendor("Win10.iso").unwrap().page.ends_with("/windows/windows-11"));
        let mut it=item("ubuntu.iso",false);it.art_color="#E95420".into();
        let restored:Item=serde_json::from_str(&serde_json::to_string(&it).unwrap()).unwrap();
        assert_eq!(restored.art_color,it.art_color);
        let mut settings:Settings=serde_json::from_value(serde_json::json!({"SteamGridDbApiKey":"obsolete","Unrelated":42})).unwrap();
        settings.normalize_appearance();let saved=serde_json::to_value(settings).unwrap();
        assert!(saved.get("SteamGridDbApiKey").is_none());assert_eq!(saved["Unrelated"],42);
    }

    #[test]
    fn program_sources_are_data_driven_and_wide_only() {
        for v in VENDORS { assert!(vendor(v.pattern.split('|').next().unwrap()).is_some()); }
        assert_eq!(vendor("SteamSetup.exe").unwrap().title,"Steam");
        assert_eq!(vendor("VSCodeUserSetup-x64.exe").unwrap().title,"Visual Studio Code");
        assert_eq!(github_repo("https://github.com/example/tool/releases/download/v1/setup.exe").as_deref(),Some("https://github.com/example/tool"));
        assert!(github_repo("https://github.com.evil.invalid/example/tool/releases/download/v1/a").is_none());
        assert_eq!(host_page("https://dl.cdn.example.invalid/file").as_deref(),Some("https://example.invalid/"));
        assert!(wide_image(800,500,800)); assert!(!wide_image(799,400,800)); assert!(!wide_image(1200,900,800));
        let html=include_str!("../../tests/fixtures/artwork/vendor.html");
        assert_eq!(social_images(html,"https://example.invalid/download"),vec!["https://example.invalid/social.jpg","https://example.invalid/twitter.png"]);
    }

    #[test]
    fn keyless_game_fixtures_require_exact_titles_and_types() {
        let doc: Value = serde_json::from_str(include_str!("../../tests/fixtures/artwork/keyless-games.json")).unwrap();
        assert_eq!(store_match(&doc["search"],"Bloodborne",true).as_deref(),Some("game-id"));
        assert_eq!(store_match(&doc["search"],"Bloodborne",false),None);
        assert_eq!(store_match(&doc["search"],"Spotify",false).as_deref(),Some("app-id"));
        assert_eq!(store_match(&doc["search"],"Spotify",true),None);
        assert_eq!(ps_match(&doc,"God of War III Remastered").as_deref(),Some("CUSA01623"));
        assert_eq!(store_match(&doc["search"],"Bloodborne III",true),None);
        assert_eq!(store_hero(&doc["details"]).as_deref(),Some("https://example.invalid/hero.jpg"));
        assert_eq!(ps_match(&doc,"Bloodborne").as_deref(),Some("CUSA00207"));
        assert_eq!(ps_match(&doc,"Bloodborne II"),None);
        assert_eq!(gog_image(&doc,"Bloodborne").as_deref(),Some("https://example.invalid/gog.jpg"));
        assert_eq!(gog_image(&doc,"Bloodborne II"),None);
        assert!(!strict_title("Game", "Game Remastered"));
        let mut it=item("App.exe",false);it.banner_file="web-ms-app-hero.jpg".into();assert!(!is_game(&it));
        it.icon_source="web:Game:artwork".into();assert!(is_game(&it));
    }

    #[test]
    fn ordinary_titles_preserve_versions_and_have_no_dangling_separators() {
        for (raw, title) in [
            ("ubuntu-26.04.1-desktop-amd64.iso", "ubuntu 26.04.1 desktop amd64"),
            ("App_v2.5.1_x64-setup.exe", "App v2.5.1 x64 setup"),
            ("Notes_2026.10.30.pdf", "Notes 2026.10.30"),
            ("Some Game -- [FitGirl Repack] --.rar", "Some Game"),
        ] { assert_eq!(clean_title(raw), title); }
        let it = item("ubuntu-26.04.1-desktop-amd64.iso", false);
        assert_eq!(display_title(&it), "ubuntu 26.04.1 desktop amd64");
        assert_eq!(it.file_name, "ubuntu-26.04.1-desktop-amd64.iso");
        let json = serde_json::to_value(Settings::default()).unwrap();
        assert!(json.get("SteamGridDbApiKey").is_none());
    }

    #[test]
    fn short_steam_queries_require_exact_titles() {
        for (raw, wrong) in [("Deck_2.5.1_x64-setup.exe", "Deck of Haunts"), ("Payload.zip", "Payload Delivery"), ("Setup.exe", "Setup Factory"), ("Installer.exe", "The Installer Game"), ("v2.5.1.exe", "Version"), ("2.5.1.zip", "2.5.1")] {
            assert!(steam_score(&clean_title(raw), wrong).is_none(), "{raw}");
        }
        assert!(steam_score("Deck", "DECK!").is_some());
        assert!(steam_score("Payload", "Payload").is_some());
        assert!(steam_score("Elden Ring", "Elden Ring Deluxe Edition").is_some());
        assert!(steam_score("Elden Ring", "Elden Ring 2").is_some());
        assert!(steam_score("RE", "Resident Evil").is_none());
    }

    #[test]
    fn cleans_site_and_multipart_titles() {
        assert_eq!(clean_title("God_of_War_III_Remastered_--_fitgirl-repacks.site_--_.part1.rar"), "God of War III Remastered");
        assert_eq!(clean_title("Deck_2.5.1_x64-setup.exe"), "Deck 2.5.1 x64 setup");
    }

    #[test]
    fn saved_steam_matches_are_revalidated_and_reset() {
        let mut it = item("Deck_2.5.1_x64-setup.exe", false);
        it.icon_source = "steam:42".into();
        it.steam_title = "Deck of Haunts".into();
        it.icon_file = "old-icon.jpg".into();
        it.cover_file = "old-cover.jpg".into();
        it.banner_file = "old-banner.jpg".into();
        it.icon_lookup_at = Some(chrono::Local::now());
        assert!(invalid_steam_match(&it));
        reset_steam_match(&mut it);
        assert!([&it.icon_source, &it.steam_title, &it.icon_file, &it.cover_file, &it.banner_file].iter().all(|s| s.is_empty()));
        assert!(it.icon_lookup_at.is_none());
        it.icon_source = "steam:42".into();
        it.steam_title = "Deck".into();
        assert!(!invalid_steam_match(&it));
        it.steam_title.clear();
        assert!(invalid_steam_match(&it));
    }

    #[test]
    fn missing_web_banners_retry_with_throttling() {
        let mut it = item("App.exe", false);
        it.icon_source = "web:Software:App".into();
        it.banner_file = "missing-banner.jpg".into();
        assert!(web_banner_due(&it));
        it.icon_lookup_at = Some(chrono::Local::now());
        assert!(!web_banner_due(&it));
        it.icon_lookup_at = Some(chrono::Local::now() - chrono::Duration::days(8));
        assert!(web_banner_due(&it));
    }

    #[test]
    fn display_titles_keep_real_filenames_and_video_titles() {
        let name = "God_of_War_III_Remastered_--_fitgirl-repacks.site_--_.part1.rar";
        let mut it = item(name, false);
        assert_eq!(display_title(&it), "God of War III Remastered");
        assert_eq!(it.file_name, name);
        it.is_video = true;
        assert_eq!(display_title(&it), name);
    }

    #[test]
    fn console_display_title_survives_saved_artwork_and_keeps_filename() {
        let name = "EP9000-PPSA21567_00-ASTROBOT000000000.pkg";
        let mut it = item(name, false);
        it.icon_source = "web:Game:Astro_Bot".into();
        assert!(console_title_due(&it));
        it.console_title = "Astro Bot".into();
        assert_eq!(display_title(&it), "Astro Bot");
        assert_eq!(it.file_name, name);
        assert!(!console_title_due(&it));
        let saved = serde_json::to_string(&it).unwrap();
        let restored: Item = serde_json::from_str(&saved).unwrap();
        assert_eq!(display_title(&restored), "Astro Bot");
    }

    #[test]
    fn social_metadata_accepts_attribute_order_and_relative_urls() {
        assert_eq!(web_page("web:Software:https://en.wikipedia.org/wiki/Deck_(software)").as_deref(), Some("https://en.wikipedia.org/wiki/Deck_(software)"));
        assert_eq!(web_page("web:Book:https://openlibrary.org/works/OL1W").as_deref(), Some("https://openlibrary.org/works/OL1W"));
        assert_eq!(web_page("web:Software:Example App").as_deref(), Some("https://en.wikipedia.org/wiki/Example_App"));
        assert_eq!(social_images(r#"<meta content='/wide.png?a=1&amp;b=2' property='og:image'><meta name="twitter:image" content="https://example.invalid/twitter.jpg"><meta property='og:title' content='Title'>"#, "https://example.invalid/page"), vec!["https://example.invalid/wide.png?a=1&b=2", "https://example.invalid/twitter.jpg"]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec(); png.extend_from_slice(&[0;8]); png.extend_from_slice(&1500u32.to_be_bytes()); png.extend_from_slice(&1000u32.to_be_bytes());
        assert_eq!(image_size(&png), Some((1500,1000)));
        assert_eq!(image_size(b"invalid"), None);
    }

    #[test]
    fn banner_prefers_library_hero_then_capsule_then_header() {
        let urls = banner_urls(42, &serde_json::json!({"header_image": "https://example.invalid/header.jpg", "capsule_image": "https://example.invalid/portrait.jpg"}));
        assert_eq!(urls, vec![
            "https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/42/library_hero.jpg",
            "https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/42/capsule_616x353.jpg",
            "https://example.invalid/header.jpg",
        ]);
    }

    #[test]
    fn banner_without_header_has_no_portrait_fallback() {
        for data in [Value::Null, serde_json::json!({"header_image": ""}), serde_json::json!({"header_image": "  ", "capsule_image": "portrait.jpg"})] {
            let urls = banner_urls(7, &data);
            assert_eq!(urls.len(), 2);
            assert!(urls[0].ends_with("/7/library_hero.jpg"));
        }
    }

    #[test]
    fn game_titles_drop_trailing_platform_words() {
        for platform in ["Windows", "PC", "Win64", "Win32", "x64", "x86", "Linux", "Portable"] {
            assert_eq!(clean_game_title(&format!("Bloodborne-{platform}.zip")), "Bloodborne");
        }
        assert_eq!(clean_game_title("Bloodborne.Windows.PC.Win64.Portable.zip"), "Bloodborne");
        assert_eq!(clean_game_title("Windows of the World.zip"), "Windows of the World");
        assert_eq!(clean_title("Ubuntu-Linux-24.04.iso"), "Ubuntu Linux 24.04");
    }

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
        assert_eq!(clean_game_title("Dune.Part.Two.2024.1080p.WEB-DL.DDP5.1.Atmos.H.264-FLUX"), "Dune Part Two 2024");
        assert_eq!(clean_game_title("Breaking.Bad.S01E02.720p.BluRay.x264-YIFY"), "Breaking Bad");
        assert_eq!(words(&clean_title("ubuntu-24.04.1-desktop-amd64.iso")), vec!["ubuntu", "24", "04", "1", "desktop", "amd64"]);
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

#[cfg(test)]
mod banner_fit_tests {
    use super::*;

    fn save(img: image::RgbaImage, name: &str) -> String {
        let dir = std::env::current_dir().unwrap().join("target").join(format!("banner-fit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        img.save(&p).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn padded_share_images_are_cropped_or_turned_down() {
        // A square picture in the middle of a white 1200x630 share image: cropped to 600x600, then too square.
        let mut img = image::RgbaImage::from_pixel(1200, 630, image::Rgba([255, 255, 255, 255]));
        for y in 15..615 { for x in 300..900 { img.put_pixel(x, y, image::Rgba([(x % 255) as u8, 40, (y % 255) as u8, 255])); } }
        let p = save(img, "share.png");
        assert_eq!(fit_banner(&p, true), None, "boxed square art is not a banner");
        let kept = fit_banner(&p, false).unwrap();
        assert!(kept.ends_with("-fit.png"));
        assert_eq!(image::open(&kept).unwrap().width(), 600);
    }

    #[test]
    fn real_wide_banners_pass_unchanged() {
        // Wide art with a plain dark sky along the top only: not padding, kept whole.
        let mut img = image::RgbaImage::from_pixel(1920, 620, image::Rgba([10, 12, 20, 255]));
        for y in 200..620 { for x in 0..1920 { img.put_pixel(x, y, image::Rgba([(x % 200) as u8, (y % 200) as u8, 90, 255])); } }
        let p = save(img, "hero.png");
        assert_eq!(fit_banner(&p, true).as_deref(), Some(p.as_str()));
        // Too small to fill the header.
        let small = save(image::RgbaImage::from_fn(460, 215, |x, y| image::Rgba([(x % 255) as u8, (y % 255) as u8, 7, 255])), "small.png");
        assert_eq!(fit_banner(&small, true), None);
    }

    #[test]
    fn store_square_art_prefers_large_box_art() {
        let doc = serde_json::json!({"Products":[{"LocalizedProperties":[{"Images":[
            {"ImagePurpose":"Logo","Width":300,"Height":300,"Uri":"//store-images.example/logo.png"},
            {"ImagePurpose":"BoxArt","Width":1080,"Height":1080,"Uri":"//store-images.example/box.png"},
            {"ImagePurpose":"SuperHeroArt","Width":3840,"Height":2160,"Uri":"//store-images.example/hero.jpg"}]}]}]});
        assert_eq!(store_square(&doc).as_deref(), Some("https://store-images.example/box.png"));
        assert_eq!(store_square(&serde_json::json!({})), None);
    }

    #[test]
    fn icon_crops_follow_the_detailed_part_of_a_wide_picture() {
        // Plain wide picture with busy "title" detail near the right edge: the square lands there, not in the middle.
        let mut img = image::RgbaImage::from_pixel(900, 300, image::Rgba([30, 30, 40, 255]));
        for y in 0..300 { for x in 600..900 { if (x / 6 + y / 6) % 2 == 0 { img.put_pixel(x, y, image::Rgba([250, 250, 250, 255])); } } }
        let p = save(img, "wide-cover.png");
        let sq = square_icon(&p).unwrap();
        let out = image::open(&sq).unwrap().to_rgba8();
        assert_eq!(out.dimensions(), (300, 300));
        let bright = out.pixels().filter(|px| px[0] > 200).count();
        assert!(bright > 300 * 300 / 3, "the square holds the detailed part ({bright} bright pixels)");
        // Detail at both ends (a face left, a logo right): zoomed out so both stay in the icon.
        let mut both = image::RgbaImage::from_pixel(533, 300, image::Rgba([30, 30, 40, 255]));
        for y in 0..300 { for x in (20..150).chain(383..513) { if (x / 6 + y / 6) % 2 == 0 { both.put_pixel(x, y, image::Rgba([250, 250, 250, 255])); } } }
        let b = image::open(square_icon(&save(both, "both.png")).unwrap()).unwrap().to_rgba8();
        let left = (0..100).flat_map(|x| (100..200).map(move |y| (x, y))).filter(|&(x, y)| b.get_pixel(x, y)[0] > 200).count();
        let right = (200..300).flat_map(|x| (100..200).map(move |y| (x, y))).filter(|&(x, y)| b.get_pixel(x, y)[0] > 200).count();
        assert!(left > 500 && right > 500, "both ends visible: left {left}, right {right}");
        // Square pictures are left alone.
        let square = save(image::RgbaImage::from_pixel(200, 200, image::Rgba([1, 2, 3, 255])), "square.png");
        assert!(square_icon(&square).is_none());
    }

    #[test]
    fn console_words_count_next_to_underscores_and_side_titles_are_skipped() {
        assert!(HAS_GAME_WORDS.is_match("CONTROL Resonant_ Deluxe Edition (v0.563 + 6 DLCs_PS5 DLC Unlocker)"));
        assert!(HAS_GAME_WORDS.is_match("Game-PS4-Repack"));
        assert!(!HAS_GAME_WORDS.is_match("Spswitchboard"));
        assert!(SIDE.is_match("Control Resonant (Original Soundtrack)"));
    }
}