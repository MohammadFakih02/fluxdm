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
const FFMPEG_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

pub const FORMATS: [(&str, &str); 9] = [
    ("best", "Best available"),
    ("2160", "4K (2160p) MP4"),
    ("1440", "1440p MP4"),
    ("1080", "1080p MP4"),
    ("720", "720p MP4"),
    ("480", "480p MP4"),
    ("360", "360p MP4"),
    ("audio-mp3", "Audio only — MP3"),
    ("audio-m4a", "Audio only — M4A"),
];

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
        fetch(YTDLP_URL, &exe_path(), |p| status(format!("installing yt-dlp {p:.0}%"))).await?;
    }
    if !ffmpeg_path().exists() {
        let zip = tools_dir().join("ffmpeg.zip");
        fetch(FFMPEG_URL, &zip, |p| status(format!("installing ffmpeg {p:.0}%"))).await?;
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
    match it.video_format.as_str() {
        "audio-mp3" => add(&["-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", "0"]),
        "audio-m4a" => add(&["-f", "ba[ext=m4a]/ba/b", "-x", "--audio-format", "m4a"]),
        h if h.parse::<i32>().is_ok() => add(&["-f", &format!("bv*[height<={h}]+ba/b[height<={h}]"), "-S", "res,ext:mp4:m4a", "--merge-output-format", "mp4"]),
        _ => add(&["-f", "bv*+ba/b", "--merge-output-format", "mp4/mkv"]),
    }
    let folder = PathBuf::from(&it.video_work_folder);
    if it.video_playlist {
        let out = folder.join("%(playlist_title).80B").join("%(playlist_index)03d - %(title).120B.%(ext)s");
        add(&["--yes-playlist", "-o", &out.to_string_lossy()]);
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
    let r: Value = serde_json::from_str(&json).map_err(|e| DlError::fatal(e.to_string()))?;
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
    }))
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
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut err = BufReader::new(child.stderr.take().unwrap()).lines();
    run.active.store(1, Ordering::Relaxed);
    mgr.with_item(&id, |it| {
        it.set_status(DlStatus::Downloading);
        it.error_message.clear();
    });

    let mut st = Progress::default();
    let (mut out_done, mut err_done) = (false, false);
    loop {
        tokio::select! {
            l = out.next_line(), if !out_done => match l { Ok(Some(l)) => st.line(&l, run), _ => out_done = true },
            l = err.next_line(), if !err_done => match l { Ok(Some(l)) => st.line(&l, run), _ => err_done = true },
            _ = run.cancel.cancelled() => {
                kill_tree(pid);
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(DlError::cancelled());
            }
        }
        if out_done && err_done {
            break;
        }
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
    if paths.is_empty() { return Err(DlError::fatal("yt-dlp didn't report an output file")); }
    let mut files = vec![];
    for source in paths {
        let source = PathBuf::from(source);
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

#[derive(Default)]
struct Progress {
    finished: i64,
    current: i64,
    stream_started: bool,
    final_paths: Vec<String>,
    last_error: String,
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
