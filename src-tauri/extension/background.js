import { ext, isGecko, loadSettings, extSet, extOf, api } from "./common.js";

// ---------------------------------------------------------------- capturing browser downloads
//
// Anything that makes the browser start a download — a download button, a link, a redirect,
// a form — shows up here. Matching files are paused, handed to FluxDM, then cancelled and
// removed from the browser's list. If FluxDM isn't running, the browser simply carries on.

const recent = new Map(); // url -> time, so a site firing the same download twice isn't sent twice

// Waterfox/Firefox report a download's *final* URL — often a signed CDN link (GitHub, Hugging Face…)
// that expires within minutes. Remember redirect chains so FluxDM gets the original, stable link and
// can obtain a fresh signed URL on every reconnect. (Chromium already reports the original in item.url.)
const redirectOrigin = new Map(); // redirected-to URL -> URL the chain started from
if (!ext.downloads.onDeterminingFilename && ext.webRequest) {
  ext.webRequest.onBeforeRedirect.addListener(d => {
    redirectOrigin.set(d.redirectUrl, redirectOrigin.get(d.url) || d.url);
    if (redirectOrigin.size > 500) redirectOrigin.delete(redirectOrigin.keys().next().value);
  }, { urls: ["<all_urls>"] });
}

// ---------------------------------------------------------------- Waterfox / Firefox: catch at the network level
//
// Gecko only fires downloads.onCreated *after* its "What should Waterfox do with this file?" dialog,
// while it is already saving to a temporary .part file — too late, and other download add-ons
// (IDM, JDownloader…) race us there. Instead, look at every page/frame response as its headers
// arrive: if it's a file download, hand it to FluxDM and cancel the request before the browser
// does anything with it. Gecko still supports blocking webRequest in Manifest V3; Chromium doesn't,
// so there we rely on onDeterminingFilename below (which fires before Chrome's Save-As prompt).

// Things the browser would normally *display* rather than download.
const DISPLAYABLE = /^(text\/|image\/|application\/(pdf|json|xml|xhtml\+xml|javascript|x-javascript|ecmascript|rss\+xml|atom\+xml)$|multipart\/)/i;

function header(d, name) {
  const h = d.responseHeaders?.find(h => h.name.toLowerCase() === name);
  return h?.value ?? "";
}

function nameFromDisposition(cd) {
  let m = /filename\*\s*=\s*[^']*'[^']*'([^;]+)/i.exec(cd);
  if (m) { try { return decodeURIComponent(m[1].trim().replace(/^"|"$/g, "")); } catch { } }
  m = /filename\s*=\s*(?:"([^"]*)"|([^;]+))/i.exec(cd);
  return m ? (m[1] ?? m[2]).trim() : "";
}

function nameFromUrl(u) {
  try { return decodeURIComponent(new URL(u).pathname.split("/").pop() || ""); } catch { return ""; }
}

async function onHeaders(d) {
  if (d.method !== "GET" || d.statusCode < 200 || d.statusCode >= 300) return {};
  if (!/^https?:/i.test(d.url) || /^https?:\/\/(127\.0\.0\.1|localhost)([:/]|$)/i.test(d.url)) return {};
  const s = await loadSettings();
  if (!s.enabled) return {};

  const cd = header(d, "content-disposition");
  const contentType = header(d, "content-type").split(";")[0].trim();
  const attachment = /^\s*attachment/i.test(cd);
  // A web page, image, PDF… that the browser would show — leave it alone unless the server forces a download.
  if (!attachment && (contentType === "" || DISPLAYABLE.test(contentType))) return {};

  const url = redirectOrigin.get(d.url) || d.url;
  const fileName = nameFromDisposition(cd) || nameFromUrl(d.url);
  const type = extOf(fileName) || extOf(url);
  const size = parseInt(header(d, "content-length"), 10) || 0;
  const wanted = s.allTypes || extSet(s).has(type) || (s.bigFileMB > 0 && size >= s.bigFileMB * 1024 * 1024);
  if (!wanted) {
    if (attachment) log(fileName || url, `left to browser — “.${type || "?"}” isn't in the file-type list`);
    return {};
  }

  const { skipNextUntil = 0 } = await ext.storage.local.get("skipNextUntil");
  if (skipNextUntil > Date.now()) {
    await ext.storage.local.remove("skipNextUntil");
    log(fileName || url, "left to browser — you chose “next download to the browser”");
    return {};
  }

  try {
    await send([url], {
      referer: d.documentUrl || d.originUrl || "",
      fileName,
      cookieUrl: url,
      storeId: d.cookieStoreId,
    }, s);
  } catch (e) {
    log(fileName || url, `FluxDM unreachable (${e?.message || e}) — left to browser`);
    flash("!", "#dc2626");
    return {}; // FluxDM isn't running: let the browser download it normally
  }
  recent.set(url, Date.now());
  recent.set(d.url, Date.now());
  log(fileName || url, "sent to FluxDM");
  flash("✓", "#16a34a");
  if (d.type === "main_frame") closeIfEmptyTab(d.tabId, d.url);
  return { cancel: true };
}

/** A download link with target=_blank leaves an empty tab behind once we take the download — close it. */
async function closeIfEmptyTab(tabId, url) {
  if (tabId < 0) return;
  setTimeout(async () => {
    try {
      const tab = await ext.tabs.get(tabId);
      if (tab.url === "about:blank" || tab.url === url) await ext.tabs.remove(tabId);
    } catch { }
  }, 400);
}

if (isGecko && ext.webRequest?.onHeadersReceived) {
  ext.webRequest.onHeadersReceived.addListener(onHeaders,
    { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] },
    ["blocking", "responseHeaders"]);
}

// ---------------------------------------------------------------- downloads API (Chromium; fallback on Gecko)

if (ext.downloads.onDeterminingFilename) {
  // Chromium: fires once headers are in, so the real file name and size are known.
  ext.downloads.onDeterminingFilename.addListener((item, suggest) => {
    suggest(); // keep the browser's own name choice; we decide asynchronously
    capture(item);
  });
} else {
  // Firefox / Waterfox: fallback for downloads the network hook can't see
  // (started by scripts or other add-ons, POST forms, …).
  ext.downloads.onCreated.addListener(item => capture(item));
}

/**
 * The real file name. Waterfox/Firefox first save into a temporary "<random 8 chars>.<ext>.part"
 * file, and that temporary path is what onCreated reports — so strip ".part" and, since the random
 * stem is meaningless, don't pass it on as the name (FluxDM detects the real one from the server).
 */
function realName(item) {
  let name = (item.filename || "").split(/[\\/]/).pop();
  let temporary = false;
  if (/\.part$/i.test(name)) {
    name = name.slice(0, -5);
    temporary = true;
  }
  return { name, temporary };
}

async function capture(item) {
  const s = await loadSettings();
  if (!s.enabled) return;
  if (item.byExtensionId && item.byExtensionId === ext.runtime.id) return;
  if (item.state && item.state !== "in_progress") return;

  const finalUrl = item.finalUrl || item.url;
  const url = redirectOrigin.get(item.url) || item.url;
  const { name, temporary } = realName(item);
  const label = (!temporary && name) || url.split(/[?#]/)[0].split("/").pop() || url;

  // blob:, data: and file: URLs only exist inside the browser — FluxDM can't fetch them.
  if (!/^https?:/i.test(url)) return log(label, "left to browser — generated by the page (not a web link)");
  if (/^https?:\/\/(127\.0\.0\.1|localhost)([:/]|$)/i.test(url)) return;

  const { skipNextUntil = 0 } = await ext.storage.local.get("skipNextUntil");
  if (skipNextUntil > Date.now()) {
    await ext.storage.local.remove("skipNextUntil");
    return log(label, "left to browser — you chose “next download to the browser”");
  }

  const type = extOf(name) || extOf(finalUrl) || extOf(url);
  const size = item.fileSize > 0 ? item.fileSize : item.totalBytes;
  const wanted = s.allTypes || extSet(s).has(type) || (s.bigFileMB > 0 && size >= s.bigFileMB * 1024 * 1024);
  if (!wanted) return log(label, `left to browser — “.${type || "?"}” isn't in the file-type list`);

  const last = recent.get(url) || recent.get(item.url);
  if (last && Date.now() - last < 15000) {
    await dropFromBrowser(item.id);
    return;
  }

  let pausedByUs = false;
  try { await ext.downloads.pause(item.id); pausedByUs = true; } catch { /* already cancelled by another add-on, or finished */ }
  try {
    await send([url], {
      referer: item.referrer || "",
      fileName: temporary ? "" : name,
      cookieUrl: url,
      storeId: item.cookieStoreId ?? (item.incognito ? "1" : undefined),
    }, s);
    recent.set(url, Date.now());
    await dropFromBrowser(item.id);
    flash("✓", "#16a34a");
    log(label, "sent to FluxDM");
  } catch (e) {
    // FluxDM not running: give the download back to the browser — but only if *we* paused it.
    // If another download add-on cancelled it, restarting it would fight that add-on.
    if (pausedByUs) {
      try {
        const [cur] = await ext.downloads.search({ id: item.id });
        if (cur && cur.paused) await ext.downloads.resume(item.id);
      } catch { }
    }
    flash("!", "#dc2626");
    log(label, `FluxDM unreachable (${e?.message || e}) — left to browser`);
  }
}

/** Keeps the last few decisions so the popup can show why a download was or wasn't caught. */
async function log(what, result) {
  const { activity = [] } = await ext.storage.local.get("activity");
  activity.unshift({ t: Date.now(), what: String(what).slice(0, 80), result });
  await ext.storage.local.set({ activity: activity.slice(0, 8) });
}

async function dropFromBrowser(id) {
  try { await ext.downloads.cancel(id); } catch { }
  try { await ext.downloads.erase({ id }); } catch { }
}

// ---------------------------------------------------------------- sending to FluxDM

async function cookieHeader(url, storeId) {
  const query = { url };
  if (storeId) query.storeId = storeId;
  let list = [];
  try {
    // Waterfox/Firefox with first-party isolation need firstPartyDomain: null to see all cookies.
    list = await ext.cookies.getAll(isGecko ? { ...query, firstPartyDomain: null } : query);
  } catch {
    try { list = await ext.cookies.getAll(query); } catch { }
  }
  return list.map(c => `${c.name}=${c.value}`).join("; ");
}

async function send(urls, { referer = "", fileName = "", cookieUrl = "", storeId, mode = "" } = {}, s) {
  s ??= await loadSettings();
  const body = { urls, referer, fileName, autostart: !!s.autostart, mode };
  if (s.sendCookies && cookieUrl) body.cookies = await cookieHeader(cookieUrl, storeId);
  if (s.sendUserAgent) body.userAgent = navigator.userAgent;
  await api("add", body, s);
}

async function sendOrWarn(urls, opts) {
  try {
    await send(urls, opts);
    flash(urls.length > 1 ? String(urls.length) : "✓", "#16a34a");
    return true;
  } catch {
    notify("FluxDM isn't running", "Start FluxDM, then try again.");
    flash("!", "#dc2626");
    return false;
  }
}

// ---------------------------------------------------------------- page link grabbing

// Runs inside the page.
function collectLinks(selectionOnly) {
  const out = new Set();
  const add = u => {
    if (/^magnet:\?/i.test(u || "")) { out.add(u); return; }
    try {
      const h = new URL(u, location.href).href;
      if (/^https?:/i.test(h)) out.add(h);
    } catch { }
  };
  if (selectionOnly) {
    const sel = getSelection();
    for (let i = 0; i < sel.rangeCount; i++)
      sel.getRangeAt(i).cloneContents().querySelectorAll("a[href]").forEach(a => add(a.getAttribute("href")));
    (sel.toString().match(/https?:\/\/[^\s"'<>]+|magnet:\?[^\s"'<>]+/gi) || []).forEach(add);
    const node = sel.anchorNode;
    const el = node && (node.nodeType === 1 ? node : node.parentElement);
    const link = el && el.closest("a[href]");
    if (link) add(link.getAttribute("href"));
  } else {
    document.querySelectorAll("a[href]").forEach(a => add(a.getAttribute("href")));
    document.querySelectorAll("video[src], audio[src], source[src]").forEach(m => add(m.src));
  }
  return [...out];
}

async function grab(tab, selectionOnly) {
  let links = [];
  try {
    const results = await ext.scripting.executeScript({ target: { tabId: tab.id }, func: collectLinks, args: [!!selectionOnly] });
    links = results?.[0]?.result ?? [];
  } catch {
    notify("Can't read this page", "The browser doesn't allow extensions on this page.");
    return 0;
  }
  if (!selectionOnly) {
    const wanted = extSet(await loadSettings());
    links = links.filter(u => /^magnet:/i.test(u) || u.endsWith(".torrent") || wanted.has(extOf(u)));
  }
  if (links.length === 0) {
    notify("No download links found", selectionOnly ? "The selection has no links." : "No links to the file types in FluxDM's list.");
    return 0;
  }
  const httpLinks = links.filter(u => /^https?:/i.test(u));
  const hosts = new Set(httpLinks.map(u => new URL(u).host));
  const ok = await sendOrWarn(links, { referer: tab.url, cookieUrl: httpLinks.length && hosts.size === 1 ? httpLinks[0] : "" });
  return ok ? links.length : 0;
}

// ---------------------------------------------------------------- context menu

ext.runtime.onInstalled.addListener(async () => {
  await ext.contextMenus.removeAll();
  ext.contextMenus.create({ id: "link", title: "Download with FluxDM", contexts: ["link"] });
  ext.contextMenus.create({ id: "media", title: "Download this media with FluxDM", contexts: ["video", "audio", "image"] });
  ext.contextMenus.create({ id: "selection", title: "Download selected links with FluxDM", contexts: ["selection"] });
  ext.contextMenus.create({ id: "page", title: "Download all links on this page with FluxDM", contexts: ["page"] });
  ext.contextMenus.create({ id: "video", title: "Download video with FluxDM (YouTube etc.)", contexts: ["page", "link", "video", "audio"] });
});

ext.contextMenus.onClicked.addListener(async (info, tab) => {
  const referer = tab?.url || info.pageUrl || "";
  const storeId = tab?.cookieStoreId ?? (tab?.incognito ? "1" : undefined);
  switch (info.menuItemId) {
    case "link":
      if (/^magnet:/i.test(info.linkUrl)) await sendOrWarn([info.linkUrl], { referer });
      else if (/^https?:/i.test(info.linkUrl)) await sendOrWarn([info.linkUrl], { referer, cookieUrl: info.linkUrl, storeId });
      break;
    case "media":
      if (/^https?:/i.test(info.srcUrl)) await sendOrWarn([info.srcUrl], { referer, cookieUrl: info.srcUrl, storeId });
      else notify("Can't send this media", "It's generated inside the page (blob/data URL), not a downloadable link.");
      break;
    case "selection":
      if (tab) await grab(tab, true);
      break;
    case "page":
      if (tab) await grab(tab, false);
      break;
    case "video": {
      // yt-dlp works from the page address; a link to another video page works too.
      const target = [info.linkUrl, info.pageUrl, tab?.url].find(u => /^https?:/i.test(u || ""));
      if (target) await sendOrWarn([target], { referer, mode: "video" });
      break;
    }
  }
});

// ---------------------------------------------------------------- popup messages

ext.runtime.onMessage.addListener((msg, sender, reply) => {
  (async () => {
    const [tab] = await ext.tabs.query({ active: true, currentWindow: true });
    if (msg.cmd === "grab" && tab) reply({ count: await grab(tab, false) });
    else if (msg.cmd === "video" && tab && /^https?:/i.test(tab.url))
      reply({ ok: await sendOrWarn([tab.url], { referer: "", mode: "video" }) });
    else if (msg.cmd === "sendPage" && tab && /^https?:/i.test(tab.url))
      reply({ ok: await sendOrWarn([tab.url], { referer: "", cookieUrl: tab.url, storeId: tab.cookieStoreId }) });
    else reply({ ok: false });
  })();
  return true; // async reply
});

// ---------------------------------------------------------------- feedback

async function refreshBadge() {
  const s = await loadSettings();
  await ext.action.setBadgeText({ text: s.enabled ? "" : "off" });
  await ext.action.setBadgeBackgroundColor({ color: "#6b7280" });
}

function flash(text, color) {
  ext.action.setBadgeBackgroundColor({ color });
  ext.action.setBadgeText({ text });
  setTimeout(refreshBadge, 2500);
}

function notify(title, message) {
  ext.notifications.create({ type: "basic", iconUrl: ext.runtime.getURL("icons/96.png"), title, message });
}

ext.storage.onChanged.addListener(changes => { if ("enabled" in changes) refreshBadge(); });
refreshBadge();
