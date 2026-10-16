//! HTTP plumbing shared by every download: one pooled client, request building, the first-byte probe
//! (size, resumability, real file name) and the error classification that decides what gets retried.

use reqwest::{header, Client, Method, RequestBuilder, Response, StatusCode};
use std::sync::LazyLock;
use std::time::Duration;

/// Why a download (or one connection of it) stopped.
#[derive(Debug, Clone)]
pub struct DlError {
    pub msg: String,
    /// The link itself is unusable (404, wrong size…): retrying it can't help.
    pub fatal: bool,
    pub status: u16,
    pub kind: ErrKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrKind {
    Http,
    Network,
    Timeout,
    Io,
    DiskFull,
    Cancelled,
    Other,
}

impl DlError {
    pub fn fatal(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), fatal: true, status: 0, kind: ErrKind::Other }
    }
    pub fn transient(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), fatal: false, status: 0, kind: ErrKind::Http }
    }
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), fatal: false, status: 0, kind: ErrKind::Timeout }
    }
    pub fn cancelled() -> Self {
        Self { msg: "cancelled".into(), fatal: false, status: 0, kind: ErrKind::Cancelled }
    }
    pub fn retryable(&self) -> bool {
        match self.kind {
            ErrKind::Http | ErrKind::Other => !self.fatal && self.kind == ErrKind::Http,
            ErrKind::Network | ErrKind::Timeout | ErrKind::Io => !self.fatal,
            ErrKind::DiskFull | ErrKind::Cancelled => false,
        }
    }
}

impl std::fmt::Display for DlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl From<reqwest::Error> for DlError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return Self::timeout("The server did not respond in time");
        }
        let mut src: &dyn std::error::Error = &e;
        let mut detail = e.to_string();
        while let Some(s) = src.source() {
            detail = s.to_string();
            src = s;
        }
        Self { msg: format!("Network error: {detail}"), fatal: false, status: 0, kind: ErrKind::Network }
    }
}

impl From<std::io::Error> for DlError {
    fn from(e: std::io::Error) -> Self {
        // ERROR_HANDLE_DISK_FULL (39) / ERROR_DISK_FULL (112)
        if matches!(e.raw_os_error(), Some(39) | Some(112)) || e.kind() == std::io::ErrorKind::StorageFull {
            return Self { msg: "Disk is full".into(), fatal: true, status: 0, kind: ErrKind::DiskFull };
        }
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            return Self { msg: "Windows denied access to the save folder".into(), fatal: true, status: 0, kind: ErrKind::Io };
        }
        let fatal = matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput);
        Self { msg: e.to_string(), fatal, status: 0, kind: ErrKind::Io }
    }
}

pub type DlResult<T> = Result<T, DlError>;

// ------------------------------------------------------------------ client

fn build_client(redirects: bool) -> Client {
    Client::builder()
        .redirect(if redirects { reqwest::redirect::Policy::limited(20) } else { reqwest::redirect::Policy::none() })
        .connect_timeout(Duration::from_secs(20))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(64)
        .tcp_nodelay(true)
        .build()
        .expect("http client")
}

/// Shared client. Redirects are followed on every request, so signed CDN links (Hugging Face, GitHub
/// releases…) are re-issued fresh on each reconnect instead of expiring.
pub static HTTP: LazyLock<Client> = LazyLock::new(|| build_client(true));
static NO_REDIRECT: LazyLock<Client> = LazyLock::new(|| build_client(false));

pub fn build(url: &str, headers: &str, ua: &str, range: Option<(i64, Option<i64>)>, method: Method) -> RequestBuilder {
    build_with(&HTTP, url, headers, ua, range, method)
}

fn build_with(client: &Client, url: &str, headers: &str, ua: &str, range: Option<(i64, Option<i64>)>, method: Method) -> RequestBuilder {
    let mut map = header::HeaderMap::new();
    let mut put = |k: &str, v: &str| {
        if let (Ok(k), Ok(v)) = (header::HeaderName::from_bytes(k.as_bytes()), header::HeaderValue::from_str(v)) {
            map.insert(k, v);
        }
    };
    put("User-Agent", ua);
    put("Accept", "*/*");
    put("Accept-Encoding", "identity");
    for raw in headers.split('\n') {
        let Some((name, value)) = raw.split_once(':') else { continue };
        let name = name.trim();
        if name.is_empty() || name.eq_ignore_ascii_case("range") || name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("host") {
            continue;
        }
        put(name, value.trim());
    }
    if let Some((from, to)) = range {
        let v = match to {
            Some(to) => format!("bytes={from}-{to}"),
            None => format!("bytes={from}-"),
        };
        put("Range", &v);
    }
    client.request(method, url).headers(map)
}

/// Turns an HTTP error status into a DlError with a plain-words message.
pub fn ensure_ok(resp: &Response) -> DlResult<()> {
    let st = resp.status();
    if st.is_success() {
        return Ok(());
    }
    let code = st.as_u16();
    let msg = match code {
        401 => "Authentication required (401) — add an Authorization header".into(),
        403 => "Access denied (403) — the link may have expired; try Refresh link".into(),
        404 => "File not found on the server (404)".into(),
        410 => "Link expired (410) — try Refresh link".into(),
        416 => "Server rejected the byte range (416)".into(),
        429 => "Server is rate-limiting connections (429)".into(),
        _ => format!("Server returned {code} {}", st.canonical_reason().unwrap_or("")).trim().to_string(),
    };
    let fatal = !matches!(code, 408 | 425 | 429) && code < 500;
    Err(DlError { msg, fatal, status: code, kind: ErrKind::Http })
}

/// "bytes 0-99/1000" → (0, 99, Some(1000))
pub fn content_range(resp: &Response) -> Option<(i64, i64, Option<i64>)> {
    let v = resp.headers().get(header::CONTENT_RANGE)?.to_str().ok()?;
    let v = v.trim().strip_prefix("bytes")?.trim();
    let (range, total) = v.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?, total.trim().parse().ok()))
}

#[derive(Debug, Clone)]
pub struct Probe {
    pub total: i64,
    pub resumable: bool,
    pub file_name: Option<String>,
    pub sha256: Option<String>,
}

/// Asks for the first byte range to learn size, resumability and the real file name.
pub async fn probe(url: &str, headers: &str, ua: &str, timeout_sec: i32) -> DlResult<Probe> {
    let fut = async {
        let sha = hugging_face_sha(url, headers, ua).await;
        let resp = build(url, headers, ua, Some((0, None)), Method::GET).send().await?;
        ensure_ok(&resp)?;
        let mut total = -1;
        let mut resumable = false;
        if resp.status() == StatusCode::PARTIAL_CONTENT {
            if let Some((_, _, Some(len))) = content_range(&resp) {
                total = len;
                resumable = true;
            }
        } else if let Some(cl) = resp.content_length() {
            total = cl as i64;
        }
        let mut name = name_from_disposition(&resp)
            .or_else(|| name_from_url(url, true))
            .or_else(|| name_from_url(resp.url().as_str(), true))
            .or_else(|| name_from_url(url, false))
            .unwrap_or_else(|| "download".into());
        if std::path::Path::new(&name).extension().is_none() {
            let mime = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
            if let Some(ext) = ext_from_mime(mime) {
                name.push_str(ext);
            }
        }
        drop(resp);
        Ok(Probe { total, resumable, file_name: Some(name), sha256: sha })
    };
    match tokio::time::timeout(Duration::from_secs(timeout_sec.max(5) as u64), fut).await {
        Ok(r) => r,
        Err(_) => Err(DlError::timeout("The server did not respond in time")),
    }
}

/// Hugging Face publishes the SHA-256 of LFS files in X-Linked-Etag on the redirect response.
async fn hugging_face_sha(url: &str, headers: &str, ua: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if host != "huggingface.co" && host != "hf.co" {
        return None;
    }
    let resp = build_with(&NO_REDIRECT, url, headers, ua, None, Method::HEAD).send().await.ok()?;
    let v = resp.headers().get("x-linked-etag")?.to_str().ok()?.trim().to_string();
    let v = v.strip_prefix("W/").unwrap_or(&v).trim_matches('"').to_lowercase();
    (v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit())).then_some(v)
}

fn name_from_disposition(resp: &Response) -> Option<String> {
    let cd = resp.headers().get(header::CONTENT_DISPOSITION)?;
    let raw = String::from_utf8_lossy(cd.as_bytes()).to_string();
    let mut plain = None;
    for part in raw.split(';') {
        let part = part.trim();
        let Some((k, v)) = part.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if k == "filename*" {
            // RFC 5987: UTF-8''name%20here
            let enc = v.splitn(3, '\'').nth(2).unwrap_or(v);
            let decoded = percent_decode(enc.trim_matches('"'));
            let clean = sanitize(&decoded);
            if !clean.is_empty() {
                return Some(clean);
            }
        } else if k == "filename" {
            plain = Some(sanitize(v.trim_matches('"')));
        }
    }
    plain.filter(|s| !s.is_empty())
}

fn ext_from_mime(mime: &str) -> Option<&'static str> {
    Some(match mime.split(';').next()?.trim().to_ascii_lowercase().as_str() {
        "application/zip" => ".zip",
        "application/pdf" => ".pdf",
        "application/x-msdownload" | "application/vnd.microsoft.portable-executable" => ".exe",
        "application/x-7z-compressed" => ".7z",
        "video/mp4" => ".mp4",
        "video/webm" => ".webm",
        "audio/mpeg" => ".mp3",
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "text/html" => ".html",
        "application/json" => ".json",
        _ => return None,
    })
}

// ------------------------------------------------------------------ names & links

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    let s = s.trim().trim_end_matches('.').to_string();
    let s: String = if s.chars().count() > 200 { s.chars().take(200).collect() } else { s };
    // Windows refuses to create files named CON, NUL, COM1… (with any extension).
    let stem = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0');
    if reserved { format!("_{s}") } else { s }
}

pub fn name_from_url(url: &str, require_extension: bool) -> Option<String> {
    let u = url::Url::parse(url.trim()).ok()?;
    let last = u.path().trim_end_matches('/').rsplit('/').next()?.to_string();
    if last.trim().is_empty() {
        return None;
    }
    let name = sanitize(&percent_decode(&last));
    if name.is_empty() || (require_extension && std::path::Path::new(&name).extension().is_none()) {
        return None;
    }
    Some(name)
}

pub fn is_http_url(s: &str) -> bool {
    url::Url::parse(s.trim()).map(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some()).unwrap_or(false)
}

pub fn is_magnet(s: &str) -> bool {
    let t = s.trim().to_ascii_lowercase();
    t.starts_with("magnet:?") && t.contains("xt=urn:")
}

/// A .torrent file on this PC.
pub fn is_torrent_file(s: &str) -> bool {
    let s = s.trim().trim_matches('"');
    !s.is_empty() && s.to_ascii_lowercase().ends_with(".torrent") && !is_http_url(s) && std::path::Path::new(s).is_file()
}

/// Magnet link, .torrent web link, or .torrent file on disk.
pub fn is_torrent_source(s: &str) -> bool {
    is_magnet(s)
        || is_torrent_file(s)
        || (is_http_url(s) && url::Url::parse(s.trim()).map(|u| u.path().to_ascii_lowercase().ends_with(".torrent")).unwrap_or(false))
}

/// Anything FluxDM can download: http(s) links, magnet links, .torrent files.
pub fn is_download_source(s: &str) -> bool {
    is_http_url(s) || is_magnet(s) || is_torrent_file(s)
}

/// Expands one numeric range such as file[01-12].zip into a batch.
pub fn expand_pattern(line: &str) -> Vec<String> {
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\[(\d+)-(\d+)\]").unwrap());
    let Some(m) = RE.captures(line) else { return vec![line.to_string()] };
    let (Ok(a), Ok(b)) = (m[1].parse::<i64>(), m[2].parse::<i64>()) else { return vec![line.to_string()] };
    if (b - a).abs() > 5000 {
        return vec![line.to_string()];
    }
    let whole = m.get(0).unwrap();
    let first = &m[1];
    let pad = if first.len() > 1 && first.starts_with('0') { first.len() } else { 0 };
    let step = if a <= b { 1 } else { -1 };
    let mut out = vec![];
    let mut i = a;
    loop {
        out.push(format!("{}{:0pad$}{}", &line[..whole.start()], i, &line[whole.end()..], pad = pad));
        if i == b {
            break;
        }
        i += step;
    }
    out
}

pub fn looks_downloadable(url: &str, extensions: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    let path = percent_decode(u.path());
    let Some(ext) = std::path::Path::new(&path).extension().map(|e| e.to_string_lossy().to_lowercase()) else { return false };
    extensions.split([' ', ',', ';']).filter(|e| !e.is_empty()).any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&ext))
}

/// The display name inside a magnet link (dn=…), if it has one.
pub fn magnet_name(magnet: &str) -> Option<String> {
    let q = magnet.trim().splitn(2, '?').nth(1)?;
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("dn=") {
            let v = percent_decode(&v.replace('+', " "));
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

pub fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string())).unwrap_or_else(|| "video".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_ranges() {
        assert_eq!(expand_pattern("https://a/p[01-03].zip"), vec!["https://a/p01.zip", "https://a/p02.zip", "https://a/p03.zip"]);
        assert_eq!(expand_pattern("https://a/p[3-1].zip").len(), 3);
        assert_eq!(expand_pattern("https://a/p.zip"), vec!["https://a/p.zip"]);
    }

    #[test]
    fn names() {
        assert_eq!(name_from_url("https://a/b/My%20File.zip?x=1", true).as_deref(), Some("My File.zip"));
        assert_eq!(name_from_url("https://a/b/", true), None);
        assert_eq!(sanitize("a:b?.txt."), "a_b_.txt");
        assert_eq!(sanitize("con.txt"), "_con.txt");
        assert_eq!(sanitize("COM1"), "_COM1");
        assert_eq!(sanitize("console.txt"), "console.txt");
        assert!(is_magnet("magnet:?xt=urn:btih:abc&dn=x"));
        assert_eq!(magnet_name("magnet:?xt=urn:btih:abc&dn=Hello+World").as_deref(), Some("Hello World"));
        assert!(looks_downloadable("https://x/y/file.ZIP", "zip rar"));
    }
}
