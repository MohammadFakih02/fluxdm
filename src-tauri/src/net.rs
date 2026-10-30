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
    Changed,
    Conflict,
    NoResume,
    Other,
}

pub fn is_disk_full(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(39) | Some(112)) || e.kind() == std::io::ErrorKind::StorageFull
}

impl DlError {
    pub fn disk_full(folder: &str) -> Self {
        let drive = std::path::Path::new(folder).components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
        let detail = crate::win::free_space(folder).map(|n| format!(" ({} free on {})", crate::model::fmt::size(n), drive)).unwrap_or_default();
        Self { msg: format!("The disk is full{detail}. Free some space, then resume."), fatal: true, status: 0, kind: ErrKind::DiskFull }
    }

    pub fn apply_download_error(&self, it: &mut crate::model::Item) {
        it.error_message = if self.kind == ErrKind::DiskFull { Self::disk_full(it.storage_folder()).msg } else { self.msg.clone() };
        it.set_status(if matches!(self.kind, ErrKind::DiskFull | ErrKind::Changed | ErrKind::NoResume | ErrKind::Conflict) {
            crate::model::DlStatus::Paused
        } else { crate::model::DlStatus::Error });
    }

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
            ErrKind::DiskFull | ErrKind::Cancelled | ErrKind::Changed | ErrKind::NoResume | ErrKind::Conflict => false,
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
        if is_disk_full(&e) {
            return Self::disk_full("");
        }
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            return Self { msg: "The destination already exists. Choose another name or folder before resuming.".into(), fatal: true, status: 0, kind: ErrKind::Conflict };
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

const LOCAL_PROXY_BYPASS: &str = "localhost,127.0.0.0/8,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,169.254.0.0/16,::1,fc00::/7,fe80::/10";

fn system_proxy_for(server: &str, scheme: &str) -> Option<String> {
    let endpoint = if server.contains('=') {
        server.split(';').filter_map(|v| v.split_once('=')).find(|(key, _)| key.trim().eq_ignore_ascii_case(scheme))?.1.trim()
    } else { server.trim() };
    if endpoint.is_empty() { return None; }
    Some(if endpoint.contains("://") { endpoint.to_string() } else { format!("http://{endpoint}") })
}

fn environment_proxy(bypass_local: bool) -> Option<reqwest::Proxy> {
    let read = |key: &str| std::env::var(key).ok().or_else(|| std::env::var(key.to_lowercase()).ok()).filter(|v| !v.is_empty());
    let http = read("HTTP_PROXY"); let https = read("HTTPS_PROXY"); let all = read("ALL_PROXY");
    if http.is_none() && https.is_none() && all.is_none() { return None; }
    let mut bypass = read("NO_PROXY").unwrap_or_default();
    if bypass_local { bypass.push(','); bypass.push_str(LOCAL_PROXY_BYPASS); }
    Some(reqwest::Proxy::custom(move |url| {
        if bypass_local && url.host_str().is_some_and(|h| !h.contains('.') && !h.contains(':')) { return None; }
        match url.scheme() { "http" => http.as_ref().or(all.as_ref()), "https" => https.as_ref().or(all.as_ref()), _ => None }.cloned()
    }).no_proxy(reqwest::NoProxy::from_string(&bypass)))
}

fn windows_proxy(bypass_local: bool) -> Option<reqwest::Proxy> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings").ok()?;
    if key.get_value::<u32, _>("ProxyEnable").ok()? == 0 { return None; }
    let server = key.get_value::<String, _>("ProxyServer").ok()?;
    let overrides = key.get_value::<String, _>("ProxyOverride").unwrap_or_default();
    let short_local = overrides.split(';').any(|v| v.trim() == "<local>");
    let mut bypass = overrides.split(';').filter(|v| v.trim() != "<local>").collect::<Vec<_>>().join(",");
    if bypass_local { bypass.push(','); bypass.push_str(LOCAL_PROXY_BYPASS); }
    Some(reqwest::Proxy::custom(move |url| {
        if (short_local || bypass_local) && url.host_str().is_some_and(|h| !h.contains('.') && !h.contains(':')) { return None; }
        system_proxy_for(&server, url.scheme())
    }).no_proxy(reqwest::NoProxy::from_string(&bypass)))
}

fn build_client(redirects: bool, settings: &crate::model::Settings) -> Result<Client, String> {
    let mut builder = Client::builder();
    match settings.proxy_mode.as_str() {
        "None" => builder = builder.no_proxy(),
        "System" => { if let Some(proxy) = environment_proxy(settings.proxy_bypass_local).or_else(|| windows_proxy(settings.proxy_bypass_local)) { builder = builder.proxy(proxy); } },
        "HTTP" | "SOCKS5" => {
            let endpoint = proxy_url(settings)?.ok_or("Proxy host is required")?;
            let mut proxy = reqwest::Proxy::all(endpoint).map_err(|_| "Invalid proxy address")?;
            if settings.proxy_bypass_local {
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(LOCAL_PROXY_BYPASS));
            }
            builder = builder.no_proxy().proxy(proxy);
        },
        _ => return Err("Choose a valid proxy type".into()),
    }
    builder
        .redirect(if redirects { reqwest::redirect::Policy::limited(20) } else { reqwest::redirect::Policy::none() })
        .connect_timeout(Duration::from_secs(20))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(64)
        .tcp_nodelay(true)
        .build()
        .map_err(|_| "Could not configure the HTTP client".into())
}

/// Shared client. Redirects are followed on every request, so signed CDN links (Hugging Face, GitHub
/// releases…) are re-issued fresh on each reconnect instead of expiring.
pub struct SharedHttp;
pub static HTTP: SharedHttp = SharedHttp;
static CLIENTS: LazyLock<parking_lot::RwLock<(Client, Client)>> = LazyLock::new(|| {
    let s = crate::model::Settings::default();
    parking_lot::RwLock::new((build_client(true, &s).expect("http client"), build_client(false, &s).expect("http client")))
});
impl SharedHttp {
    pub fn get(&self, url: impl reqwest::IntoUrl) -> RequestBuilder { CLIENTS.read().0.get(url) }
    pub fn post(&self, url: impl reqwest::IntoUrl) -> RequestBuilder { CLIENTS.read().0.post(url) }
}
pub fn configure_proxy(s: &crate::model::Settings) -> Result<(), String> {
    let clients = (build_client(true, s)?, build_client(false, s)?);
    *CLIENTS.write() = clients;
    Ok(())
}
pub fn proxy_url(s: &crate::model::Settings) -> Result<Option<String>, String> {
    if !matches!(s.proxy_mode.as_str(), "HTTP" | "SOCKS5") { return Ok(None); }
    let host = s.proxy_host.trim();
    if host.is_empty() || host.contains(['/', '@', '?', '#', '\\']) || s.proxy_port == 0 {
        return Err("Enter a proxy host and a port from 1 to 65535".into());
    }
    let scheme = if s.proxy_mode == "SOCKS5" { "socks5h" } else { "http" };
    let host = host.trim_matches(['[', ']']);
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    let mut url = url::Url::parse(&format!("{scheme}://{host}:{}", s.proxy_port)).map_err(|_| "Invalid proxy host")?;
    if url.host_str().is_none() { return Err("Invalid proxy host".into()); }
    if !s.proxy_username.is_empty() {
        url.set_username(&s.proxy_username).map_err(|_| "Invalid proxy login")?;
        url.set_password(Some(&s.proxy_password)).map_err(|_| "Invalid proxy login")?;
    }
    Ok(Some(url.into()))
}

pub fn build(url: &str, headers: &str, ua: &str, range: Option<(i64, Option<i64>)>, method: Method) -> RequestBuilder {
    build_with(&CLIENTS.read().0, url, headers, ua, range, method)
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

/// Custom request headers belong only to the original origin, including across redirects.
pub async fn get_scoped(source: &str, headers: &str, ua: &str) -> DlResult<Response> {
    let origin = url::Url::parse(source).map_err(|e| DlError::fatal(e.to_string()))?;
    let mut current = origin.clone();
    for _ in 0..20 {
        let resp = scoped_get(&origin, &current, headers, ua).send().await?;
        if !resp.status().is_redirection() { return Ok(resp); }
        let next = resp.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).ok_or_else(|| DlError::fatal("Redirect has no location"))?;
        current = current.join(next).map_err(|e| DlError::fatal(e.to_string()))?;
        if !matches!(current.scheme(), "http" | "https") { return Err(DlError::fatal("Unsupported redirect")); }
    }
    Err(DlError::fatal("Too many redirects"))
}

fn scoped_get(origin: &url::Url, current: &url::Url, headers: &str, ua: &str) -> RequestBuilder {
    let scoped = if current.origin() == origin.origin() { headers } else { "" };
    build_with(&CLIENTS.read().1, current.as_str(), scoped, ua, None, Method::GET)
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

/// Reject expiry and login pages before touching destination data.
pub fn ensure_file(resp: &Response, file_name: &str) -> DlResult<()> {
    let ext = std::path::Path::new(file_name).extension().and_then(|v| v.to_str()).unwrap_or("");
    let mime = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("")
        .split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if matches!(mime.as_str(), "text/html" | "application/xhtml+xml") && !ext.eq_ignore_ascii_case("html") && !ext.eq_ignore_ascii_case("htm") {
        return Err(DlError { msg: "The link returned a web page instead of the file (expired, or it needs a login). Refresh the link.".into(), fatal: true, status: 0, kind: ErrKind::Changed });
    }
    Ok(())
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
    pub url: String,
    pub total: i64,
    pub resumable: bool,
    pub file_name: Option<String>,
    pub sha256: Option<String>,
    pub validator: Option<String>,
}

pub fn validator(headers: &header::HeaderMap) -> Option<String> {
    let etag = headers.get(header::ETAG).and_then(|v| v.to_str().ok()).map(str::trim);
    etag.filter(|v| v.starts_with('"') && v.ends_with('"'))
        .or_else(|| headers.get(header::LAST_MODIFIED).and_then(|v| v.to_str().ok()))
        .map(str::to_string)
}

pub fn validator_changed(saved: Option<&str>, current: Option<&str>) -> bool {
    matches!((saved, current), (Some(a), Some(b)) if a.starts_with('"') == b.starts_with('"') && a != b)
}

pub fn range_changed(saved: Option<&str>, status: StatusCode, headers: &header::HeaderMap) -> bool {
    let current = if saved.is_some_and(|v| !v.starts_with('"')) {
        headers.get(header::LAST_MODIFIED).and_then(|v| v.to_str().ok()).map(str::to_string)
    } else {
        validator(headers)
    };
    saved.is_some() && (status == StatusCode::OK || validator_changed(saved, current.as_deref()))
}

pub fn if_range(req: RequestBuilder, validator: Option<&str>) -> RequestBuilder {
    match validator {
        Some(v) => req.header(header::IF_RANGE, v),
        None => req,
    }
}

/// Asks for the first byte range to learn size, resumability and the real file name.
pub async fn probe(url: &str, headers: &str, ua: &str, timeout_sec: i32) -> DlResult<Probe> {
    probe_file(url, headers, ua, timeout_sec, &name_from_url(url, false).unwrap_or_default()).await
}

pub async fn probe_file(url: &str, headers: &str, ua: &str, timeout_sec: i32, file_name: &str) -> DlResult<Probe> {
    let fut = async {
        let sha = hugging_face_sha(url, headers, ua).await;
        let (resp, fallback) = match build(url, headers, ua, Some((0, None)), Method::GET).send().await {
            Ok(resp) => (resp, false),
            Err(e) if reset_or_eof(&e) => (build(url, headers, ua, None, Method::GET).send().await?, true),
            Err(e) => return Err(e.into()),
        };
        ensure_ok(&resp)?;
        ensure_file(&resp, file_name)?;
        let mut total = -1;
        let mut resumable = false;
        if resp.status() == StatusCode::PARTIAL_CONTENT {
            if let Some((_, _, Some(len))) = content_range(&resp) {
                total = len;
                resumable = !fallback;
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
        let validator = validator(resp.headers());
        drop(resp);
        Ok(Probe { url: url.to_string(), total, resumable, file_name: Some(name), sha256: sha, validator })
    };
    match tokio::time::timeout(Duration::from_secs(timeout_sec.max(5) as u64), fut).await {
        Ok(r) => r,
        Err(_) => Err(DlError::timeout("The server did not respond in time")),
    }
}

fn reset_or_eof(e: &reqwest::Error) -> bool {
    let mut source: &dyn std::error::Error = e;
    loop {
        if let Some(io) = source.downcast_ref::<std::io::Error>() {
            if matches!(io.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::UnexpectedEof) { return true; }
        }
        let detail = source.to_string().to_ascii_lowercase();
        if detail.contains("connection closed before message completed") || detail.contains("unexpected eof") { return true; }
        match source.source() { Some(next) => source = next, None => return false }
    }
}

/// Hugging Face publishes the SHA-256 of LFS files in X-Linked-Etag on the redirect response.
async fn hugging_face_sha(url: &str, headers: &str, ua: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if host != "huggingface.co" && host != "hf.co" {
        return None;
    }
    let request = build_with(&CLIENTS.read().1, url, headers, ua, None, Method::HEAD);
    let resp = request.send().await.ok()?;
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
    fn torrent_fetch_headers_only_follow_same_origin_redirects() {
        let origin = url::Url::parse("https://example.invalid/private.torrent").unwrap();
        let headers = "Cookie: session=secret\nAuthorization: Bearer secret\nReferer: https://example.invalid/page\nX-Token: secret";
        for destination in ["https://example.invalid/next", "https://cdn.invalid/next", "http://example.invalid/next", "https://example.invalid:444/next"] {
            let current = url::Url::parse(destination).unwrap();
            let req = scoped_get(&origin, &current, headers, "test").build().unwrap();
            for name in ["cookie", "authorization", "referer", "x-token"] {
                assert_eq!(req.headers().contains_key(name), origin.origin() == current.origin());
            }
            assert_eq!(req.method(), Method::GET);
        }
    }

    #[test]
    fn system_proxy_rules_choose_protocol_and_preserve_bypass_defaults() {
        assert_eq!(system_proxy_for("http=one.invalid:80;https=two.invalid:443", "https").as_deref(), Some("http://two.invalid:443"));
        assert_eq!(system_proxy_for("proxy.invalid:8080", "https").as_deref(), Some("http://proxy.invalid:8080"));
        assert_eq!(system_proxy_for("http=one.invalid:80", "https"), None);
        assert_eq!(system_proxy_for("", "http"), None);
        assert!(!crate::model::Settings::default().proxy_bypass_local);
    }

    #[test]
    fn proxy_settings_validate_hosts_and_escape_login_without_leaking_it() {
        let mut s = crate::model::Settings { proxy_mode: "SOCKS5".into(), proxy_host: "::1".into(), proxy_port: 1080,
            proxy_username: "a@b".into(), proxy_password: "p:/@ word".into(), ..Default::default() };
        let endpoint = proxy_url(&s).unwrap().unwrap();
        assert!(endpoint.starts_with("socks5h://a%40b:")); assert!(endpoint.contains("@[::1]:1080"));
        assert!(build_client(true, &s).is_ok());
        for bad in ["", "host/path", "name@host", "host:80", "host?x", "host#x"] {
            s.proxy_host = bad.into(); assert!(proxy_url(&s).is_err(), "{bad}");
        }
        s.proxy_host = "example.invalid".into(); s.proxy_port = 0; assert!(proxy_url(&s).is_err());
        s.proxy_mode = "None".into(); assert_eq!(proxy_url(&s).unwrap(), None);
        s.proxy_mode = "invalid".into(); assert!(build_client(true, &s).is_err());
    }

    #[tokio::test]
    async fn custom_http_proxy_routes_requests_and_bypasses_local_addresses() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut all = Vec::new();
            loop { let mut buf = [0; 1024]; let n = stream.read(&mut buf).await.unwrap(); if n == 0 { break; }
                all.extend_from_slice(&buf[..n]); if all.windows(4).any(|v| v == b"\r\n\r\n") { break; } }
            let request = String::from_utf8(all).unwrap();
            assert!(request.starts_with("GET http://example.invalid/file"));
            assert!(request.to_lowercase().contains("proxy-authorization: basic "));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nproxy").await.unwrap();
        });
        let mut s = crate::model::Settings { proxy_mode: "HTTP".into(), proxy_host: "127.0.0.1".into(), proxy_port: port,
            proxy_username: "name".into(), proxy_password: "secret".into(), ..Default::default() };
        let response = build_client(true, &s).unwrap().get("http://example.invalid/file").send().await.unwrap();
        assert_eq!(response.text().await.unwrap(), "proxy"); task.await.unwrap();
        let local = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let addr = local.local_addr().unwrap();
        let task = tokio::spawn(async move { let (mut stream, _) = local.accept().await.unwrap(); let mut b = [0; 4096];
            let n = stream.read(&mut b).await.unwrap(); assert!(!String::from_utf8_lossy(&b[..n]).contains("secret"));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ndirect").await.unwrap(); });
        s.proxy_bypass_local = true;
        let response = build_client(true, &s).unwrap().get(format!("http://{addr}/")).send().await.unwrap();
        assert_eq!(response.text().await.unwrap(), "direct"); task.await.unwrap();
    }

    #[test]
    fn resume_validators() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::ETAG, header::HeaderValue::from_static("W/\"weak\""));
        assert_eq!(validator(&headers), None);
        headers.insert(header::LAST_MODIFIED, header::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert_eq!(validator(&headers).as_deref(), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        headers.insert(header::ETAG, header::HeaderValue::from_static("\"strong\""));
        assert_eq!(validator(&headers).as_deref(), Some("\"strong\""));
        assert!(validator_changed(Some("\"old\""), Some("\"new\"")));
        assert!(!validator_changed(None, Some("\"new\"")));
        assert!(!validator_changed(Some("\"old\""), None));
        assert!(!validator_changed(Some("\"old\""), Some("Wed, 21 Oct 2015 07:28:00 GMT")));
        assert!(!validator_changed(Some("Wed, 21 Oct 2015 07:28:00 GMT"), Some("\"strong\"")));
        assert!(range_changed(Some("\"strong\""), StatusCode::OK, &headers));
        assert!(range_changed(Some("\"old\""), StatusCode::PARTIAL_CONTENT, &headers));
        assert!(!range_changed(Some("\"strong\""), StatusCode::PARTIAL_CONTENT, &headers));
        assert!(!range_changed(None, StatusCode::OK, &headers));
        assert!(!range_changed(Some("Wed, 21 Oct 2015 07:28:00 GMT"), StatusCode::PARTIAL_CONTENT, &headers));
        assert!(range_changed(Some("Tue, 20 Oct 2015 07:28:00 GMT"), StatusCode::PARTIAL_CONTENT, &headers));
        let req = build("https://example.invalid/file", "", "test", Some((10, None)), Method::GET);
        let req = if_range(req, Some("\"strong\"")).build().unwrap();
        assert_eq!(req.headers()[header::IF_RANGE], "\"strong\"");
        assert_eq!(req.headers()[header::RANGE], "bytes=10-");
    }

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
