//! yt-dlp + ffmpeg: installs them on first use (official releases), probes pages and drives downloads,
//! reading yt-dlp's progress lines. Pausing kills yt-dlp; it resumes its own .part files next time.

use crate::manager::{Manager, Run};
use crate::model::*;
use crate::net::{self, DlError, DlResult, ErrKind};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
const YTDLP_SUMS_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS";
const FFMPEG_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

pub const FORMATS: [(&str, &str); 13] = [
    ("best", "Best available"),
    ("2160", "4K (2160p) MP4"),
    ("1440", "1440p MP4"),
    ("1080", "1080p MP4"),
    ("720", "720p MP4"),
    ("480", "480p MP4"),
    ("360", "360p MP4"),
    ("smallest", "Smallest file"),
    ("audio-mp3", "Audio only — MP3"),
    ("audio-m4a", "Audio only — M4A"),
    ("audio-opus", "Audio only — Opus"),
    ("audio-flac", "Audio only — FLAC"),
    ("custom", "Custom format (Settings → Videos)"),
];

/// Checks a playlist selection like "1,3,5-9" (what --playlist-items takes). Empty = everything.
pub fn playlist_items(s: &str) -> Result<String, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() { return Ok(s); }
    let ok = s.split(',').all(|part| {
        let mut ends = part.splitn(2, '-');
        let a = ends.next().unwrap_or("").parse::<u32>().ok().filter(|n| *n > 0);
        match (a, ends.next()) {
            (Some(_), None) => true,
            (Some(a), Some(b)) => b.parse::<u32>().is_ok_and(|b| b >= a),
            _ => false,
        }
    });
    if ok { Ok(s) } else { Err("Couldn't read the playlist selection. Use numbers and ranges like 1,3,5-9.".into()) }
}

fn seconds(t: &str) -> Option<f64> {
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() > 3 || parts.iter().any(|p| p.is_empty()) { return None; }
    let mut total = 0.0;
    for p in &parts {
        let v: f64 = p.parse().ok().filter(|v: &f64| *v >= 0.0)?;
        total = total * 60.0 + v;
    }
    Some(total)
}

/// Checks a part of a video like "1:30-2:45" or "90-120" (start before end). Empty = the whole video.
pub fn section(s: &str) -> Result<String, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() { return Ok(s); }
    let bad = || "Couldn't read the part to download. Use start-end, like 1:30-2:45 or 0:00:10-0:01:00.".to_string();
    let (a, b) = s.split_once('-').ok_or_else(bad)?;
    match (seconds(a), seconds(b)) {
        (Some(x), Some(y)) if y > x => Ok(s),
        _ => Err(bad()),
    }
}

const VIDEO_HOSTS: [&str; 30] = [
    "youtube.com", "youtu.be", "youtube-nocookie.com", "vimeo.com", "dailymotion.com", "twitch.tv", "twitter.com", "x.com",
    "tiktok.com", "instagram.com", "facebook.com", "fb.watch", "reddit.com", "v.redd.it", "soundcloud.com", "bilibili.com",
    "rumble.com", "kick.com", "streamable.com", "bandcamp.com", "odysee.com", "bitchute.com", "nicovideo.jp", "vk.com",
    "ok.ru", "mixcloud.com", "9gag.com", "threads.net", "bsky.app", "ted.com",
];

static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn tools_dir() -> PathBuf {
    crate::store::data_dir().join("tools")
}
fn exe_path() -> PathBuf {
    tools_dir().join("yt-dlp.exe")
}
fn ffmpeg_path() -> PathBuf {
    tools_dir().join("ffmpeg.exe")
}
pub fn is_installed() -> bool {
    exe_path().exists() && ffmpeg_path().exists()
}

/// A page on a known video site (not a direct file link).
pub fn is_video_site(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url.trim()) else { return false };
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    VIDEO_HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{h}"))) && net::name_from_url(url, true).is_none()
}

/// Adds a what-to-do hint to yt-dlp's most common errors.
pub fn explain(error: &str) -> String {
    let l = error.to_lowercase();
    if l.contains("confirm you") && l.contains("bot") {
        return "YouTube wants proof you're not a bot — sign in to YouTube in your browser, and set Settings → Video → Use browser cookies from.".into();
    }
    if l.contains("sign in to confirm your age") || l.contains("age-restricted") {
        return "Age-restricted video — sign in to YouTube in your browser and use Settings → Video → browser cookies.".into();
    }
    if l.contains("private video") || l.contains("members") {
        return format!("{error} — needs your browser cookies (Settings → Video).");
    }
    if l.contains("unsupported url") {
        return "yt-dlp doesn't support this site/page. Untick “Download as video” to download the link as a normal file.".into();
    }
    if l.contains("could not copy") && l.contains("cookie") {
        return "Couldn't read the browser's cookies (Chrome/Edge lock them while open) — close that browser or pick Waterfox/Firefox in Settings → Video.".into();
    }
    error.to_string()
}

// ------------------------------------------------------------------ install

pub async fn ensure_installed(status: impl Fn(String)) -> DlResult<()> {
    let _g = INSTALL.lock().await;
    std::fs::create_dir_all(tools_dir())?;
    if !exe_path().exists() {
        let new = tools_dir().join("yt-dlp.new.exe");
        fetch(YTDLP_URL, &new, |p| status(format!("installing yt-dlp {p:.0}%"))).await?;
        status("checking yt-dlp".into());
        let verified = async {
            let sums = fetch_text(YTDLP_SUMS_URL).await?;
            let want = sums.lines().find_map(|l| l.split_once(char::is_whitespace).filter(|(_, n)| n.trim().trim_start_matches('*') == "yt-dlp.exe").map(|(h, _)| h.to_string()))
                .ok_or_else(|| DlError::fatal("yt-dlp's checksum list doesn't list yt-dlp.exe"))?;
            check_sha256(&new, &want)?;
            // A file that matches but can't run (blocked by antivirus, wrong platform) isn't installed either.
            let ok = tokio::process::Command::new(&new).arg("--version").creation_flags(crate::win::CREATE_NO_WINDOW).output().await.is_ok_and(|o| o.status.success());
            if ok { Ok(()) } else { Err(DlError::fatal("The downloaded yt-dlp doesn't run (an antivirus may have blocked it)")) }
        }.await;
        if let Err(e) = verified {
            let _ = std::fs::remove_file(&new);
            return Err(e);
        }
        std::fs::rename(&new, exe_path())?;
    }
    if !ffmpeg_path().exists() {
        let zip = tools_dir().join("ffmpeg.zip");
        fetch(FFMPEG_URL, &zip, |p| status(format!("installing ffmpeg {p:.0}%"))).await?;
        status("checking ffmpeg".into());
        let verified = async {
            let want = fetch_text(&format!("{FFMPEG_URL}.sha256")).await?;
            check_sha256(&zip, want.split_whitespace().next().unwrap_or(""))
        }.await;
        if let Err(e) = verified {
            let _ = std::fs::remove_file(&zip);
            return Err(e);
        }
        status("unpacking ffmpeg".into());
        let dir = tools_dir();
        let z2 = zip.clone();
        tauri::async_runtime::spawn_blocking(move || -> std::io::Result<()> {
            let mut archive = zip::ZipArchive::new(std::fs::File::open(&z2)?).map_err(std::io::Error::other)?;
            for i in 0..archive.len() {
                let mut e = archive.by_index(i).map_err(std::io::Error::other)?;
                let name = e.name().rsplit('/').next().unwrap_or("").to_string();
                if name == "ffmpeg.exe" || name == "ffprobe.exe" {
                    let mut out = std::fs::File::create(dir.join(&name))?;
                    std::io::copy(&mut e, &mut out)?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| DlError::fatal(e.to_string()))??;
        let _ = std::fs::remove_file(&zip);
        if !ffmpeg_path().exists() {
            return Err(DlError::fatal("ffmpeg package didn't contain ffmpeg.exe"));
        }
    }
    Ok(())
}

/// A small text file (a checksum list), with the same proxy settings as everything else.
async fn fetch_text(url: &str) -> DlResult<String> {
    let resp = tokio::time::timeout(std::time::Duration::from_secs(60), net::build(url, "", "FluxDM/2.0", None, reqwest::Method::GET).send())
        .await
        .map_err(|_| DlError::timeout("The checksum list didn't arrive"))??;
    net::ensure_ok(&resp)?;
    Ok(resp.text().await?)
}

/// The file's SHA-256 must match the published one (hex, any case).
fn check_sha256(path: &Path, want: &str) -> DlResult<()> {
    use sha2::{Digest, Sha256};
    let want = want.trim().to_ascii_lowercase();
    if want.len() != 64 || !want.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DlError::fatal("The published checksum couldn't be read, so the download wasn't trusted"));
    }
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got == want { Ok(()) } else { Err(DlError::fatal("The download doesn't match its published checksum (damaged or tampered with), so it wasn't installed. Try again later.")) }
}

async fn fetch(url: &str, dest: &std::path::Path, progress: impl Fn(f64)) -> DlResult<()> {
    use tokio::io::AsyncWriteExt;
    // A connection that goes silent must fail (and free the install lock), not leave every video waiting forever.
    const STALL: std::time::Duration = std::time::Duration::from_secs(60);
    let stalled = || DlError::timeout("The download of yt-dlp/ffmpeg stopped (no data for 60 seconds)");
    let tmp = dest.with_extension("tmp");
    // ffmpeg is ~100 MB: carry on from what an earlier, interrupted attempt left behind.
    let have = std::fs::metadata(&tmp).map(|m| m.len() as i64).unwrap_or(0);
    let send = |range| tokio::time::timeout(STALL, net::build(url, "", "FluxDM/2.0", range, reqwest::Method::GET).send());
    let mut resp = send((have > 0).then_some((have, None))).await.map_err(|_| stalled())??;
    if resp.status().as_u16() == 416 {
        let _ = std::fs::remove_file(&tmp);
        resp = send(None).await.map_err(|_| stalled())??;
    }
    net::ensure_ok(&resp)?;
    let resumed = have > 0 && resp.status().as_u16() == 206;
    let start = if resumed { have as f64 } else { 0.0 };
    let total = start + resp.content_length().unwrap_or(0) as f64;
    let mut f = if resumed {
        tokio::fs::OpenOptions::new().append(true).open(&tmp).await?
    } else {
        tokio::fs::File::create(&tmp).await?
    };
    let (mut got, mut last) = (start, -1f64);
    while let Some(chunk) = tokio::time::timeout(STALL, resp.chunk()).await.map_err(|_| stalled())?? {
        f.write_all(&chunk).await?;
        got += chunk.len() as f64;
        let pct = if total > 0.0 { got * 100.0 / total } else { 0.0 };
        if pct - last >= 1.0 {
            last = pct;
            progress(pct);
        }
    }
    f.flush().await?;
    drop(f);
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

// ------------------------------------------------------------------ command lines

fn waterfox_profile() -> Option<String> {
    let root = PathBuf::from(std::env::var("APPDATA").ok()?).join("Waterfox");
    let ini = std::fs::read_to_string(root.join("profiles.ini")).ok()?;
    let lines: Vec<&str> = ini.lines().collect();
    // Newer builds record the profile in use under [Install…] Default=
    let mut rel = None;
    let mut in_install = false;
    for l in &lines {
        if l.starts_with('[') {
            in_install = l.starts_with("[Install");
        } else if in_install && l.starts_with("Default=") {
            rel = Some(l[8..].to_string());
            break;
        }
    }
    let rel = rel.or_else(|| lines.iter().find(|l| l.starts_with("Path=")).map(|l| l[5..].to_string()))?;
    let full = root.join(rel.replace('/', "\\"));
    full.is_dir().then(|| full.to_string_lossy().into_owned())
}

fn find_on_path(exe: &str) -> Option<String> {
    std::env::var("PATH").ok()?.split(';').map(|d| PathBuf::from(d.trim()).join(exe)).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
}

fn base_args(s: &Settings) -> Vec<String> {
    let mut a: Vec<String> = vec!["--no-colors".into(), "--encoding".into(), "utf-8".into(), "--ffmpeg-location".into(), tools_dir().to_string_lossy().into_owned()];
    // YouTube needs a JavaScript runtime; yt-dlp looks for Deno by default, so point it at Node if that's what's here.
    if find_on_path("deno.exe").is_none() {
        if let Some(node) = find_on_path("node.exe") {
            a.push("--js-runtimes".into());
            a.push(format!("node:{node}"));
        }
    }
    // A cookies.txt file, when set, replaces the browser.
    if !s.video_cookies_file.trim().is_empty() && Path::new(s.video_cookies_file.trim()).is_file() {
        a.push("--cookies".into());
        a.push(s.video_cookies_file.trim().to_string());
        return a;
    }
    let mut b = s.video_cookies_browser.to_lowercase();
    if b.starts_with("auto") {
        // Waterfox first (the .NET build's choice), then Firefox; Chromium browsers lock their cookie DB while open.
        let firefox = std::env::var("APPDATA").map(|a| std::path::Path::new(&a).join("Mozilla\\Firefox\\Profiles").is_dir()).unwrap_or(false);
        b = if waterfox_profile().is_some() { "waterfox".into() } else if firefox { "firefox".into() } else { "none".into() };
    }
    match b.as_str() {
        "waterfox" => {
            if let Some(p) = waterfox_profile() {
                a.push("--cookies-from-browser".into());
                a.push(format!("firefox:{p}"));
            }
        }
        "firefox" | "chrome" | "edge" | "brave" | "opera" | "vivaldi" => {
            a.push("--cookies-from-browser".into());
            a.push(b);
        }
        _ => {}
    }
    a
}

fn command(args: &[String]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(exe_path());
    cmd.args(args)
        .current_dir(tools_dir())
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(crate::win::CREATE_NO_WINDOW)
        .kill_on_drop(true);
    cmd
}

fn download_args(it: &Item, s: &Settings, limit_bps: i64) -> Vec<String> {
    let mut a = base_args(s);
    let mut add = |xs: &[&str]| a.extend(xs.iter().map(|x| x.to_string()));
    add(&["--newline", "--progress", "--no-mtime", "--windows-filenames", "--no-warnings"]);
    add(&["--progress-template", "download:FLUXPROG %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s"]);
    add(&["--print", "after_move:FLUXFILE %(filepath)s"]);
    add(&["-N", &if limit_bps > 0 { 1 } else { it.connections.clamp(1, 16) }.to_string()]);
    if limit_bps > 0 {
        add(&["--limit-rate", &limit_bps.to_string()]);
    }
    for line in it.headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Referer") { add(&["--referer", value.trim()]); }
            else if name.trim().eq_ignore_ascii_case("User-Agent") { add(&["--user-agent", value.trim()]); }
        }
    }
    let audio = it.video_format.starts_with("audio-");
    match it.video_format.as_str() {
        "audio-mp3" => add(&["-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", "0"]),
        "audio-m4a" => add(&["-f", "ba[ext=m4a]/ba/b", "-x", "--audio-format", "m4a", "--audio-quality", "0"]),
        "audio-opus" => add(&["-f", "ba/b", "-x", "--audio-format", "opus", "--audio-quality", "0"]),
        "audio-flac" => add(&["-f", "ba/b", "-x", "--audio-format", "flac"]),
        "smallest" => add(&["-f", "bv*+ba/b", "-S", "+size,+br,+res,+fps", "--merge-output-format", "mp4/mkv"]),
        "custom" if !s.video_custom_format.trim().is_empty() => add(&["-f", s.video_custom_format.trim(), "--merge-output-format", "mp4/mkv"]),
        h if h.parse::<i32>().is_ok() => add(&["-f", &format!("bv*[height<={h}]+ba/b[height<={h}]"), "-S", "res,ext:mp4:m4a", "--merge-output-format", "mp4"]),
        _ => add(&["-f", "bv*+ba/b", "--merge-output-format", "mp4/mkv"]),
    }
    if !audio && s.video_subtitles != "None" {
        add(&[if s.video_subtitles == "Embed" { "--embed-subs" } else { "--write-subs" }, "--sub-langs", &s.video_sub_langs]);
        if s.video_auto_subs { add(&["--write-auto-subs"]); }
    }
    if s.video_embed_meta {
        add(&["--embed-metadata", "--embed-chapters", "--embed-thumbnail"]);
    }
    const SPONSOR: &str = "sponsor,selfpromo,interaction,intro,outro";
    match s.video_sponsorblock.as_str() {
        "Mark" => add(&["--sponsorblock-mark", SPONSOR]),
        "Remove" => add(&["--sponsorblock-remove", SPONSOR]),
        _ => {}
    }
    if !it.video_section.is_empty() {
        add(&["--download-sections", &format!("*{}", it.video_section)]);
    }
    if s.video_live_from_start { add(&["--live-from-start"]); }
    if s.video_wait_for_live { add(&["--wait-for-video", "60"]); }
    if s.video_use_archive {
        add(&["--download-archive", &crate::store::data_dir().join("video-archive.txt").to_string_lossy()]);
    }
    let folder = PathBuf::from(&it.video_work_folder);
    if it.video_playlist {
        let out = folder.join("%(playlist_title).80B").join("%(playlist_index)03d - %(title).120B.%(ext)s");
        add(&["--yes-playlist", "-o", &out.to_string_lossy()]);
        if !it.video_playlist_items.is_empty() { add(&["--playlist-items", &it.video_playlist_items]); }
    } else {
        let out = folder.join("%(title).150B [%(id)s].%(ext)s");
        add(&["--no-playlist", "-o", &out.to_string_lossy()]);
    }
    add(&["--", &it.url]);
    a
}

/// What the Add window shows about a video page.
pub async fn probe(url: &str, s: &Settings) -> DlResult<Value> {
    ensure_installed(|_| {}).await?;
    let mut args = base_args(s);
    args.extend(["-J", "--flat-playlist", "--no-warnings", "--", url].iter().map(|x| x.to_string()));
    // A page that never answers must not leave the Add window "reading" forever (kill_on_drop stops yt-dlp).
    let out = match tokio::time::timeout(std::time::Duration::from_secs(90), command(&args).output()).await {
        Ok(r) => r?,
        Err(_) => return Err(DlError::fatal("yt-dlp didn't answer within 90 seconds — check the link and your connection")),
    };
    let json = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || json.trim().is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| l.starts_with("ERROR:")).map(|l| l[6..].trim().to_string());
        return Err(DlError::fatal(explain(&line.unwrap_or_else(|| "yt-dlp couldn't read this page".into()))));
    }
    let r = parse_probe(&json).ok_or_else(|| {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| l.starts_with("ERROR:")).map(|l| l[6..].trim().to_string());
        DlError::fatal(explain(&line.unwrap_or_else(|| "yt-dlp's answer couldn't be read".into())))
    })?;
    let playlist = r["_type"] == "playlist";
    let mut heights: Vec<i64> = vec![];
    let mut approx = 0i64;
    if let Some(fs) = r["formats"].as_array() {
        for f in fs {
            if let Some(h) = f["height"].as_i64() {
                if !heights.contains(&h) {
                    heights.push(h);
                }
            }
            for k in ["filesize", "filesize_approx"] {
                if let Some(v) = f[k].as_i64() {
                    approx = approx.max(v);
                }
            }
        }
    }
    heights.sort_unstable_by(|a, b| b.cmp(a));
    // Approximate download size per resolution: the biggest (best) video stream at that height plus the best
    // audio stream, since that's the pair yt-dlp merges. Sizes missing from the page are estimated from the bitrate.
    let duration = r["duration"].as_f64().unwrap_or(0.0);
    let size_of = |f: &Value| -> i64 {
        for k in ["filesize", "filesize_approx"] {
            if let Some(v) = f[k].as_i64().filter(|v| *v > 0) {
                return v;
            }
        }
        match f["tbr"].as_f64() {
            Some(t) if t > 0.0 && duration > 0.0 => (t * 1000.0 / 8.0 * duration) as i64,
            _ => 0,
        }
    };
    let mut audio = 0i64;
    let mut by_height: Vec<(i64, i64)> = vec![];
    if !playlist {
        for f in r["formats"].as_array().into_iter().flatten() {
            let (v, a) = (f["vcodec"].as_str().unwrap_or("none"), f["acodec"].as_str().unwrap_or("none"));
            let s = size_of(f);
            if v == "none" && a != "none" {
                audio = audio.max(s);
            } else if v != "none" && f["height"].as_i64().is_some() && !f["format_note"].as_str().unwrap_or("").contains("storyboard") {
                let h = f["height"].as_i64().unwrap_or(0);
                match by_height.iter_mut().find(|e| e.0 == h) {
                    Some(e) => e.1 = e.1.max(s),
                    None => by_height.push((h, s)),
                }
            }
        }
        by_height.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    }
    let sizes: Vec<Value> = by_height
        .iter()
        .filter(|(h, _)| *h >= 100)
        .map(|(h, s)| serde_json::json!({ "height": h, "size": if *s > 0 { *s + audio } else { 0 } }))
        .collect();
    // MP3 at yt-dlp's best quality is roughly 245 kbps; M4A keeps the source audio stream as it is.
    let mp3 = if duration > 0.0 { (245.0 * 1000.0 / 8.0 * duration) as i64 } else { 0 };
    Ok(serde_json::json!({
        "sizes": sizes,
        "audioSize": audio,
        "mp3Size": mp3,
        "title": r["title"].as_str().unwrap_or(""),
        "duration": r["duration"].as_f64().unwrap_or(0.0),
        "heights": heights,
        "approxSize": approx,
        "site": r["extractor_key"].as_str().unwrap_or(""),
        "playlist": playlist,
        "entries": if playlist { r["entries"].as_array().map(|e| e.len()).unwrap_or(0) } else { 0 },
        // For the playlist picker (numbered from 1, like --playlist-items).
        "entryList": if playlist { r["entries"].as_array().into_iter().flatten().take(1000).enumerate()
            .map(|(i, e)| serde_json::json!({ "n": i + 1, "title": e["title"].as_str().unwrap_or(""), "duration": e["duration"].as_f64().unwrap_or(0.0) }))
            .collect::<Vec<_>>() } else { vec![] },
        "id": r["id"].as_str().unwrap_or(""),
        // "is_live" | "is_upcoming" | "was_live" | "not_live" | "" (unknown)
        "liveStatus": r["live_status"].as_str().unwrap_or(""),
    }))
}

/// The JSON object in yt-dlp's output, even when a warning or notice was printed before or after it.
fn parse_probe(out: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(out.trim()) { return v.is_object().then_some(v); }
    let (a, b) = (out.find('{')?, out.rfind('}')?);
    if b <= a { return None; }
    serde_json::from_str::<Value>(&out[a..=b]).ok().filter(Value::is_object)
}

/// Runs "yt-dlp -U" and returns its output.
pub async fn update() -> String {
    if !exe_path().exists() {
        return "yt-dlp isn't installed yet — it installs itself with your first video download.".into();
    }
    match command(&["-U".to_string()]).output().await {
        Ok(o) => format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_string(),
        Err(e) => e.to_string(),
    }
}

pub async fn version() -> String {
    if !exe_path().exists() {
        return "not installed (installs automatically on first video download)".into();
    }
    match command(&["--version".to_string()]).output().await {
        Ok(o) => format!(
            "yt-dlp {}{}",
            String::from_utf8_lossy(&o.stdout).trim(),
            if ffmpeg_path().exists() { " · ffmpeg installed" } else { " · ffmpeg missing" }
        ),
        Err(e) => e.to_string(),
    }
}

// ------------------------------------------------------------------ runner

/// Every yt-dlp process (and the ffmpeg it starts) joins one Windows job that ends with FluxDM: if FluxDM
/// crashes or is ended from Task Manager, Windows closes the job and stops them instead of leaving them
/// downloading and holding files open.
fn tie_to_app(pid: u32) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::*;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
    static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let Ok(h) = CreateJobObjectW(None, windows::core::PCWSTR::null()) else { return 0 };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = SetInformationJobObject(h, JobObjectExtendedLimitInformation, &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
        if set.is_err() { let _ = CloseHandle(h); return 0; }
        h.0 as usize
    });
    if job == 0 || pid == 0 { return; }
    unsafe {
        if let Ok(p) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) {
            let _ = AssignProcessToJobObject(HANDLE(job as *mut _), p);
            let _ = CloseHandle(p);
        }
    }
}

fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(crate::win::CREATE_NO_WINDOW)
        .output();
}

use std::os::windows::process::CommandExt as _;

/// All intermediate files live in one private job directory; final files are recorded individually.
pub fn cleanup(it: &Item, recycle_outputs: bool) -> std::io::Result<()> {
    let work = PathBuf::from(&it.video_work_folder);
    if !it.video_work_folder.is_empty() && (!work.is_absolute() || work.file_name().and_then(|n| n.to_str()) != Some(&format!(".fluxdm-{}", it.id))) {
        return Err(std::io::Error::other("Invalid video staging folder"));
    }
    for path in &it.video_files {
        match remove_output(Path::new(path), recycle_outputs) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if it.video_playlist {
            if let Some(parent) = std::path::Path::new(path).parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
    if !it.video_work_folder.is_empty() {
        match std::fs::remove_dir_all(work) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    } else if it.video_files.is_empty() && it.status == DlStatus::Completed {
        // Older jobs only have their last completed file's path; other files have no ownership record.
        match remove_output(Path::new(&it.full_path()), recycle_outputs) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn remove_output(path: &Path, recycle: bool) -> std::io::Result<()> {
    if !recycle { return std::fs::remove_file(path); }
    if !path.try_exists()? { return Ok(()); }
    crate::win::recycle(&path.to_string_lossy());
    if path.try_exists()? { return Err(std::io::Error::other("Couldn't move a video output to the Recycle Bin")); }
    Ok(())
}

pub async fn run(mgr: Arc<Manager>, run: Arc<Run>) {
    let id = run.id.clone();
    let r = go(&mgr, &run).await;
    run.active.store(0, Ordering::Relaxed);
    let done = run.current.load(Ordering::Relaxed);
    match r {
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

async fn go(mgr: &Arc<Manager>, run: &Arc<Run>) -> DlResult<()> {
    let id = run.id.clone();
    mgr.with_item(&id, |it| {
        it.set_status(DlStatus::Connecting);
        it.error_message.clear();
        it.last_try = Some(chrono::Local::now());
    });
    if !is_installed() {
        let m = mgr.clone();
        let i = id.clone();
        tokio::select! {
            r = ensure_installed(move |s| { m.with_item(&i, |it| it.error_message = s); }) => r?,
            _ = run.cancel.cancelled() => return Err(DlError::cancelled()),
        }
    }
    mgr.with_item(&id, |it| {
        if it.video_work_folder.is_empty() {
            it.video_work_folder = PathBuf::from(&it.save_folder).join(format!(".fluxdm-{}", it.id)).to_string_lossy().into_owned();
        }
    });
    // Save ownership before starting the child, so cleanup also works after an interrupted session.
    mgr.save();
    let Some(it) = mgr.item(&id) else { return Ok(()) };
    std::fs::create_dir_all(&it.save_folder)?;
    std::fs::create_dir_all(&it.video_work_folder)?;
    let s = mgr.settings();
    let limit = run.limiter.get();
    run.started_limit.store(limit, Ordering::Relaxed);

    let mut child = command(&download_args(&it, &s, limit)).spawn()?;
    let pid = child.id().unwrap_or(0);
    tie_to_app(pid);
    // Split on newlines rather than `lines()`: a line that isn't valid UTF-8 (an odd title or folder name)
    // must not end the reading early and lose the progress and the output paths.
    let mut out = BufReader::new(child.stdout.take().unwrap()).split(b'\n');
    let mut err = BufReader::new(child.stderr.take().unwrap()).split(b'\n');
    run.active.store(1, Ordering::Relaxed);
    mgr.with_item(&id, |it| {
        it.set_status(DlStatus::Downloading);
        it.error_message.clear();
    });

    let mut st = Progress::default();
    let (mut out_done, mut err_done) = (false, false);
    let mut shown_note = String::new();
    loop {
        tokio::select! {
            l = out.next_segment(), if !out_done => match l { Ok(Some(l)) => st.line(&String::from_utf8_lossy(&l), run), _ => out_done = true },
            l = err.next_segment(), if !err_done => match l { Ok(Some(l)) => st.line(&String::from_utf8_lossy(&l), run), _ => err_done = true },
            _ = run.cancel.cancelled() => {
                kill_tree(pid);
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(DlError::cancelled());
            }
        }
        if st.note != shown_note {
            shown_note = st.note.clone();
            mgr.with_item(&id, |it| it.note = shown_note.clone());
        }
        if out_done && err_done {
            break;
        }
    }
    if !shown_note.is_empty() {
        mgr.with_item(&id, |it| it.note.clear());
    }
    let status = tokio::select! {
        s = child.wait() => s?,
        _ = run.cancel.cancelled() => { kill_tree(pid); let _ = child.wait().await; return Err(DlError::cancelled()); }
    };
    run.active.store(0, Ordering::Relaxed);
    if !status.success() {
        let msg = if st.last_error.is_empty() { format!("yt-dlp failed (exit code {})", status.code().unwrap_or(-1)) } else { explain(&st.last_error) };
        return Err(DlError::transient(msg));
    }
    if st.final_paths.is_empty() && st.archived {
        return Err(DlError::fatal("Already downloaded before (it's in the download archive). To get it again, turn off Settings → Videos → Don't download the same video twice."));
    }
    let (size, target) = publish_outputs(mgr, &it, &st.final_paths)?;
    mgr.with_item(&id, |it| {
        it.save_folder = target.parent().unwrap().to_string_lossy().into_owned();
        it.file_name = target.file_name().unwrap().to_string_lossy().into_owned();
        it.total_size = size;
        it.downloaded = size;
        it.error_message.clear();
        it.set_status(DlStatus::Completed);
        it.completed_at = Some(chrono::Local::now());
    });
    Ok(())
}

fn publish_outputs(mgr: &Manager, it: &Item, paths: &[String]) -> DlResult<(i64, PathBuf)> {
    let work = PathBuf::from(&it.video_work_folder);
    // yt-dlp can print a path in a different encoding than the one on disk (non-Latin titles or folders). The
    // staging folder belongs to this job alone, so when a reported path doesn't exist, its finished files are
    // the outputs.
    let mut found: Vec<PathBuf> = paths.iter().map(PathBuf::from).filter(|p| p.is_file()).collect();
    if found.len() < paths.len() || found.is_empty() {
        let mut staged = finished_files(&work);
        staged.retain(|p| !found.contains(p));
        found.extend(staged);
    }
    if found.is_empty() { return Err(DlError::fatal("yt-dlp didn't report an output file")); }
    found.sort();
    let mut files = vec![];
    for source in found {
        let relative = source.strip_prefix(&work).map_err(|_| DlError::fatal("Video output is outside its staging folder"))?;
        if relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) { return Err(DlError::fatal("Invalid video output path")); }
        let destination = PathBuf::from(&it.save_folder).join(relative);
        let folder = destination.parent().ok_or_else(|| DlError::fatal("Invalid video output folder"))?;
        std::fs::create_dir_all(folder)?;
        let name = mgr.unique_name(&folder.to_string_lossy(), &destination.file_name().unwrap().to_string_lossy(), Some(&it.id), &[]);
        let destination = folder.join(name);
        std::fs::rename(&source, &destination)?;
        mgr.with_item(&it.id, |it| it.video_files.push(destination.to_string_lossy().into_owned()));
        mgr.save();
        files.push(destination);
    }
    let size: i64 = files.iter().map(|p| std::fs::metadata(p).map(|m| m.len() as i64)).collect::<Result<Vec<_>, _>>()?.iter().sum();
    let target = if it.video_playlist { files[0].parent().unwrap().to_path_buf() } else { files[0].clone() };
    let _ = std::fs::remove_dir_all(&work);
    Ok((size, target))
}

/// Completed files in a staging folder (not partial downloads, fragments or yt-dlp's own state files).
fn finished_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(finished_files(&p));
            continue;
        }
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let partial = [".part", ".ytdl", ".temp", ".tmp"].iter().any(|x| name.ends_with(x))
            || name.contains(".part-frag") || name.contains(".temp.") || name.ends_with(".f4v.part");
        // Intermediate streams yt-dlp merges and then deletes (video.f137.mp4) only count if nothing else is there.
        if !partial { out.push(p); }
    }
    let merged: Vec<PathBuf> = out.iter().filter(|p| !is_format_stream(p)).cloned().collect();
    if merged.is_empty() { out } else { merged }
}

/// "name.f137.mp4" - one stream before merging.
fn is_format_stream(p: &Path) -> bool {
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    stem.rsplit_once('.').is_some_and(|(_, f)| f.len() > 1 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

#[derive(Default)]
struct Progress {
    finished: i64,
    current: i64,
    stream_started: bool,
    final_paths: Vec<String>,
    last_error: String,
    /// Shown under the name: waiting for a scheduled stream, recording live, post-processing.
    note: String,
    /// yt-dlp skipped it because it's in the download archive.
    archived: bool,
}

impl Progress {
    fn line(&mut self, line: &str, run: &Run) {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("FLUXPROG ") {
            let p: Vec<&str> = rest.split(' ').collect();
            let num = |i: usize| p.get(i).and_then(|v| v.parse::<f64>().ok()).map(|v| v as i64).unwrap_or(-1);
            let d = num(0);
            let mut t = num(1);
            if t <= 0 {
                t = num(2);
            }
            if d < 0 {
                return;
            }
            if self.note.starts_with("waiting") {
                self.note = if t <= 0 { "live - recording".into() } else { String::new() };
            } else if self.note == "finishing the file" {
                self.note.clear();
            }
            if d < self.current {
                // yt-dlp moved on to the next stream (e.g. video done, now audio).
                self.finished += self.current;
                self.current = 0;
                self.stream_started = false;
            }
            // The first report of a stream may include bytes resumed from a .part file — not speed.
            if self.stream_started {
                run.bytes.fetch_add((d - self.current).max(0), Ordering::Relaxed);
            }
            self.stream_started = true;
            self.current = d;
            run.current.store(self.finished + self.current, Ordering::Relaxed);
            if t > 0 {
                run.total_hint.store(self.finished + t, Ordering::Relaxed);
            }
        } else if let Some(p) = line.strip_prefix("FLUXFILE ") {
            let path = p.trim().to_string();
            if !self.final_paths.contains(&path) { self.final_paths.push(path); }
        } else if let Some(e) = line.strip_prefix("ERROR:") {
            self.last_error = e.trim().to_string();
        } else if line.contains("has already been recorded in the archive") {
            self.archived = true;
        } else if line.starts_with("[wait]") || line.contains("Waiting for video") {
            self.note = "waiting for the stream to start".into();
        } else if line.contains("live from start") || (line.starts_with("[download]") && line.contains("(frag ") && self.note.starts_with("waiting")) {
            self.note = "live - recording".into();
        } else if ["[Merger]", "[ExtractAudio]", "[EmbedSubtitle]", "[EmbedThumbnail]", "[Metadata]", "[SponsorBlock]", "[ModifyChapters]", "[FixupM3u8]"]
            .iter().any(|p| line.starts_with(p)) {
            self.note = "finishing the file".into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video() -> (PathBuf, Item) {
        let root = std::env::current_dir().unwrap().join("target").join(format!("video-test-{}", uuid::Uuid::new_v4()));
        let mut it = Item { is_video: true, save_folder: root.to_string_lossy().into_owned(), ..Default::default() };
        it.video_work_folder = root.join(format!(".fluxdm-{}", it.id)).to_string_lossy().into_owned();
        std::fs::create_dir_all(&it.video_work_folder).unwrap();
        (root, it)
    }

    #[test]
    fn probe_json_survives_extra_lines() {
        let v = parse_probe("WARNING: something\n{\"id\":\"abc\",\"title\":\"x\"}\nupdate available\n").unwrap();
        assert_eq!(v["id"], "abc");
        assert!(parse_probe("no json here").is_none());
        assert!(parse_probe("[1,2]").is_none());
    }

    #[test]
    fn playlist_and_section_inputs_are_checked() {
        assert_eq!(playlist_items(" 1, 3,5-9 ").unwrap(), "1,3,5-9");
        assert_eq!(playlist_items("").unwrap(), "");
        for bad in ["0", "a", "5-2", "1,,2", "-3"] { assert!(playlist_items(bad).is_err(), "{bad}"); }
        assert_eq!(section("1:30-2:45").unwrap(), "1:30-2:45");
        assert_eq!(section("90-120").unwrap(), "90-120");
        assert_eq!(section("0:00:10 - 0:01:00").unwrap(), "0:00:10-0:01:00");
        for bad in ["2:00-1:00", "abc", "1:2:3:4-5", "10"] { assert!(section(bad).is_err(), "{bad}"); }
    }

    #[test]
    fn new_options_reach_yt_dlp() {
        let (root, mut it) = video();
        let mut s = Settings { video_subtitles: "Embed".into(), video_auto_subs: true, video_sponsorblock: "Remove".into(), video_use_archive: true, ..Default::default() };
        it.video_format = "audio-opus".into();
        it.video_section = "1:00-2:00".into();
        it.video_playlist = true;
        it.video_playlist_items = "2,4".into();
        let a = download_args(&it, &s, 0).join(" ");
        assert!(a.contains("--audio-format opus") && a.contains("--download-sections *1:00-2:00") && a.contains("--playlist-items 2,4"), "{a}");
        assert!(a.contains("--sponsorblock-remove") && a.contains("--download-archive") && a.contains("--embed-metadata"), "{a}");
        assert!(!a.contains("--embed-subs"), "audio files get no subtitles: {a}");
        it.video_format = "best".into();
        s.video_embed_meta = false;
        let a = download_args(&it, &s, 0).join(" ");
        assert!(a.contains("--embed-subs") && a.contains("--write-auto-subs") && !a.contains("--embed-thumbnail"), "{a}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn outputs_are_found_when_the_reported_path_is_garbled() {
        let (root, it) = video();
        let work = PathBuf::from(&it.video_work_folder);
        std::fs::write(work.join("أغنية 🎵 [id].mp4"), b"video").unwrap();
        std::fs::write(work.join("other.mp4.part"), b"partial").unwrap();
        std::fs::write(work.join("أغنية 🎵 [id].f137.mp4"), b"stream").unwrap();
        let found = finished_files(&work);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].to_string_lossy().ends_with("[id].mp4"));
        let mgr = Manager::new();
        mgr.st.lock().items.push(it.clone());
        let (size, target) = publish_outputs(&mgr, &it, &["C:\\garbled\\???.mp4".into()]).unwrap();
        assert_eq!(size, 5);
        assert!(target.is_file() && target.starts_with(&root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_removes_job_fragments_and_outputs_but_keeps_other_files() {
        let (root, mut it) = video();
        for name in ["video.mp4.part", "video.mp4.ytdl", "video.mp4.part-Frag1", "video.f137.mp4", "video.mp4"] {
            std::fs::write(PathBuf::from(&it.video_work_folder).join(name), b"partial").unwrap();
        }
        let output = root.join("finished.mp4");
        std::fs::write(&output, b"output").unwrap();
        std::fs::write(root.join("unrelated.mp4"), b"keep").unwrap();
        it.video_files.push(output.to_string_lossy().into_owned());
        let saved: Item = serde_json::from_str(&serde_json::to_string(&it).unwrap()).unwrap();
        cleanup(&saved, false).unwrap();
        cleanup(&saved, false).unwrap();
        assert!(!PathBuf::from(&it.video_work_folder).exists());
        assert!(!output.exists());
        assert_eq!(std::fs::read(root.join("unrelated.mp4")).unwrap(), b"keep");
        it.video_work_folder = root.to_string_lossy().into_owned();
        assert!(cleanup(&it, false).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn playlist_outputs_sum_sizes_target_folder_and_keep_existing_files() {
        let (root, mut it) = video();
        it.video_playlist = true;
        let staging = PathBuf::from(&it.video_work_folder).join("Playlist");
        std::fs::create_dir_all(&staging).unwrap();
        let mgr = Manager::new();
        mgr.st.lock().items.push(it.clone());
        let run = Run::test_run(&it.id);
        let mut progress = Progress::default();
        for (name, bytes) in [("one.mp4", "one"), ("two.mp4", "second")] {
            let path = staging.join(name);
            std::fs::write(&path, bytes).unwrap();
            progress.line(&format!("FLUXFILE {}", path.display()), &run);
            progress.line(&format!("FLUXFILE {}", path.display()), &run);
        }
        assert_eq!(progress.final_paths.len(), 2);
        let folder = root.join("Playlist");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("one.mp4"), b"existing").unwrap();
        let (size, target) = publish_outputs(&mgr, &it, &progress.final_paths).unwrap();
        assert_eq!(size, 9);
        assert_eq!(target, folder);
        let saved = mgr.item(&it.id).unwrap();
        assert_eq!(saved.video_files.len(), 2);
        assert!(saved.video_files.iter().all(|p| PathBuf::from(p).is_file()));
        cleanup(&saved, false).unwrap();
        assert_eq!(std::fs::read(folder.join("one.mp4")).unwrap(), b"existing");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limited_video_uses_one_fragment_worker_and_its_exact_share() {
        let it = Item { video_work_folder: "staging".into(), connections: 8, headers: "Referer: https://example.invalid/page\nUser-Agent: test-agent".into(), ..Default::default() };
        let settings = Settings { video_cookies_browser: "none".into(), ..Default::default() };
        let args = download_args(&it, &settings, 1536);
        let value = |key: &str| args[args.iter().position(|a| a == key).unwrap() + 1].as_str();
        assert_eq!(value("--limit-rate"), "1536");
        assert_eq!(value("-N"), "1");
        assert_eq!(value("--referer"), "https://example.invalid/page");
        assert_eq!(value("--user-agent"), "test-agent");
        let tiny = download_args(&it, &settings, 102);
        assert_eq!(tiny[tiny.iter().position(|a| a == "--limit-rate").unwrap() + 1], "102");
        let unlimited = download_args(&it, &settings, 0);
        assert!(!unlimited.iter().any(|a| a == "--limit-rate"));
        assert_eq!(unlimited[unlimited.iter().position(|a| a == "-N").unwrap() + 1], "8");
    }

    #[test]
    fn legacy_cleanup_only_deletes_a_known_completed_file() {
        let (root, mut it) = video();
        std::fs::remove_dir(&it.video_work_folder).unwrap();
        it.video_work_folder.clear();
        it.file_name = "old.mp4".into();
        let path = PathBuf::from(it.full_path());
        std::fs::write(&path, b"keep while unowned").unwrap();
        cleanup(&it, false).unwrap();
        assert!(path.exists());
        it.status = DlStatus::Completed;
        cleanup(&it, false).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn removing_an_unfinished_video_cleans_staging_even_without_delete_files() {
        let (root, it) = video();
        std::fs::write(PathBuf::from(&it.video_work_folder).join("video.mp4.part"), b"partial").unwrap();
        let mgr = Manager::new();
        mgr.st.lock().items.push(it.clone());
        mgr.remove(&it.id, false).await;
        assert!(mgr.item(&it.id).is_none());
        assert!(!PathBuf::from(&it.video_work_folder).exists());
        std::fs::remove_dir_all(root).unwrap();

        let (root, mut it) = video();
        it.status = DlStatus::Completed;
        let output = root.join("finished.mp4");
        std::fs::write(&output, b"keep").unwrap();
        it.video_files.push(output.to_string_lossy().into_owned());
        mgr.st.lock().items.push(it.clone());
        mgr.remove(&it.id, false).await;
        assert!(mgr.item(&it.id).is_none());
        assert_eq!(std::fs::read(output).unwrap(), b"keep");
        std::fs::remove_dir_all(root).unwrap();
    }
}
