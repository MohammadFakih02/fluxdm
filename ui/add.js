// The Add download window: links → probe → options → add_submit.
"use strict";

const A = {
  init: null, settings: {}, art: null, artName: "",
  cls: null, // last classify result
  probe: null, probedUrl: null,
  video: null,
  torrent: null, torrentFiles: [], tree: null, openDirs: new Set(), grown: false,
  expanded: false,
  nameEdited: false, folderEdited: false, videoTouched: false,
  gen: 0, // bumps on every refresh so late answers are ignored
};

const win = T.window.getCurrentWindow();
const urlsBox = $("#urls"), nameBox = $("#name"), folderBox = $("#folder"), info = $("#info");
let limitKBps = 0, upKBps = 0, seedKBps = 0;
speedCombo($("#limitBox"), { get: () => limitKBps, set: (k) => (limitKBps = k) });
// Torrents only: this torrent's own upload limits (0 = only the ones in Settings apply, -1 = no upload).
speedCombo($("#upBox"), { get: () => upKBps, set: (k) => (upKBps = k), presets: UPLOAD_PRESETS, allowNone: true });
speedCombo($("#seedBox"), { get: () => seedKBps, set: (k) => (seedKBps = k), presets: UPLOAD_PRESETS, allowNone: true });
$("#expand").innerHTML = icon("chev");
$("#copyLink").innerHTML = icon("copy");
$("#addGlyph").innerHTML = icon("down");
$("#linkGlyph").innerHTML = icon("link");
$("#startGlyph").innerHTML = icon("down");

(async function init() {
  let ready = false;
  async function appendLinks() {
    if (!ready) return;
    const links = await invoke("add_take_links");
    if (!links.trim()) return;
    urlsBox.value = [urlsBox.value.trim(), links.trim()].filter(Boolean).join("\n");
    autoGrow();
    refresh();
  }
  await win.listen("add-links", appendLinks);
  const d = (A.init = await invoke("add_init"));
  const st = (await invoke("get_settings")).settings;
  A.settings = st; applyAppearance(st);
  folderBox.value = d.defaultFolder;
  $("#conn").value = d.connections;
  $("#connVal").textContent = d.connections;
  $("#format").replaceChildren(...d.formats.map((f) => el("option", { value: f.key, text: f.label })));
  $("#format").value = d.videoFormat;
  if ($("#format").selectedIndex < 0) $("#format").selectedIndex = 0;
  $("#queue").replaceChildren(el("option", { value: "", text: "None, start now" }), ...d.queues.map((q) => el("option", { value: q, text: q })));
  const r = d.request;
  $("#queue").value = d.queues.includes(d.lastQueue) ? d.lastQueue : "";
  $("#queue").addEventListener("change", () => invoke("remember_add_queue", { name: $("#queue").value }));
  $("#headers").value = r.headers || "";
  let urls = (r.urls || "").trim();
  if (!urls) urls = (await invoke("clipboard_url")) || "";
  urlsBox.value = urls;
  autoGrow();
  if (r.video) { $("#video").checked = true; A.videoTouched = true; }
  if (!r.video && r.name) { nameBox.value = r.name; A.nameEdited = true; }
  applyModes();
  refresh(); updateFreeSpace();
  urlsBox.focus();
  urlsBox.setSelectionRange(0, 0);
  toFirstLine();
  ready = true;
  await appendLinks();
})();

// The link box is one line; the arrow opens it to every line (wrapped). A batch shows how many links it holds.
function autoGrow() {
  const lines = urlsBox.value.split("\n").filter((l) => l.trim()).length;
  $("#linkCount").hidden = A.expanded || lines < 2;
  $("#linkCount").textContent = `${lines} links`;
  $("#linkHint").hidden = !A.expanded;
  urlsBox.style.height = "";
  if (A.expanded) urlsBox.style.height = Math.min(160, Math.max(32, urlsBox.scrollHeight + 2)) + "px";
}
/** Collapsed, the box shows the start of the first link. */
function toFirstLine() { if (!A.expanded) { urlsBox.scrollTop = 0; urlsBox.scrollLeft = 0; } }
urlsBox.addEventListener("blur", toFirstLine);
function setExpanded(on) {
  A.expanded = on;
  $("#add").classList.toggle("expanded", on);
  $("#expand").title = on ? "Back to one line" : "Show every line";
  autoGrow();
  toFirstLine();
}

const torrentMode = () => !!(A.cls && A.cls.torrent);
const videoMode = () => $("#video").checked && !torrentMode();

function applyModes() {
  const t = torrentMode(), v = videoMode();
  $("#add").classList.toggle("torrent", t);
  $("#video").disabled = t;
  $("#videoPanel").hidden = !v;
  showPicker();
  if (v) $("#advanced").open = true;
  if (!t) { $("#torrentPanel").hidden = true; }
  const single = A.cls ? A.cls.urls.length === 1 : true;
  nameBox.disabled = t || v || !single;
  $("#hash").disabled = t || v;
  $("#mirrors").disabled = t || v;
  $("#conn").disabled = t;
  if (v && +$("#conn").value > 8) { $("#conn").value = 8; $("#connVal").textContent = 8; }
}

function setName(n) { nameBox.value = n || ""; updateAutoFolder(); renderDetected(); lookupArt(); }

async function updateAutoFolder() {
  if (!A.folderEdited && A.init.categorySubfolders && nameBox.value.trim() && !videoMode()) {
    const name = nameBox.value.trim(), gen = A.gen;
    const folder = await invoke("folder_for", { name });
    if (gen === A.gen && !A.folderEdited && name === nameBox.value.trim()) folderBox.value = folder;
  }
  updateFreeSpace();
}

function setInfo(lines) {
  renderDetected();
  if (A.probe || A.torrent) lines = lines.filter(l=>l && (l.startsWith("⚠") || l.includes("SHA-256")));
  info.replaceChildren(...lines.filter(Boolean).flatMap((l, i) => [i ? "\n" : "", l.startsWith("⚠") ? el("span", { class: "warn", text: l }) : l]));
}

async function freeWarning(size) {
  if (!(size > 0)) return null;
  const free = await invoke("free_space", { folder: folderBox.value.trim() || A.init.defaultFolder });
  return free != null && free < size ? `⚠ Only ${fmtSize(free)} free on that drive.` : null;
}

let timer = 0;
function refreshSoon() { clearTimeout(timer); timer = setTimeout(refresh, 450); }

async function refresh() {
  const gen = ++A.gen;
  A.art = null; A.artName = ""; applySurfaceArt(null,A.settings,$("#windowArt"));
  A.probe = null;
  A.video = null; A.torrent = null;
  const cls = await invoke("classify", { text: urlsBox.value });
  if (gen !== A.gen) return;
  A.cls = cls;
  const urls = cls.urls;
  if (!cls.torrent) { A.torrent = null; A.torrentFiles = []; }
  if (!A.videoTouched && urls.length && !cls.torrent && cls.video !== $("#video").checked) $("#video").checked = cls.video;
  applyModes();
  $("#start").disabled = $("#later").disabled = urls.length === 0;

  if (!urls.length) {
    setInfo([urlsBox.value.trim() ? "That doesn't look like an http(s) or magnet link." : "Paste a link to see its size and name."]);
    return;
  }
  if (cls.torrent) return refreshTorrent(gen, urls);
  if (videoMode()) return refreshVideo(gen, urls);

  if (urls.length > 1) {
    setName("");
    const dups = cls.dups.length;
    setInfo([`${urls.length} links — each file's name is detected when it starts.`, dups ? `⚠ ${dups} of them are already in your list.` : null]);
    return;
  }
  const url = urls[0];
  if (!A.nameEdited) setName(cls.names[0] || "");
  setInfo(["Fetching file info…"]);
  try {
    const p = await invoke("probe", { url, headers: $("#headers").value });
    if (gen !== A.gen) return;
    A.probe = p;
    A.probedUrl = url;
    if (!A.nameEdited && p.fileName) setName(p.fileName);
    if (p.sha256 && !$("#hash").value.trim()) $("#hash").value = p.sha256;
    const warning = await freeWarning(p.total);
    if (gen !== A.gen) return;
    setInfo([
      `Size: ${p.total > 0 ? fmtSize(p.total) : "unknown"}`,
      p.resumable ? "Resumable: yes — multi-connection download, survives pauses and reboots" : "Resumable: no — single connection; pausing restarts it from zero",
      p.sha256 ? "SHA-256 published by Hugging Face — the file will be verified when it finishes." : null,
      warning,
      cls.dups[0] ? `⚠ Already in your list (${cls.dups[0]}).` : null,
    ]);
  } catch (e) {
    if (gen === A.gen) setInfo([`Couldn't fetch info: ${e}`, "You can still add it — FluxDM retries when it starts."]);
  }
}

/** Narrows the quality list to what this video really offers, each with its approximate size. */
function applyVideoFormats(v) {
  const sel = $("#format");
  if (v.playlist || !v.sizes || !v.sizes.length) return;
  const want = sel.value;
  const tag = (n) => (n > 0 ? ` — ~${fmtSize(n)}` : "");
  const opts = [{ key: "best", label: `Best available (${v.sizes[0].height}p)${tag(v.sizes[0].size)}` }];
  for (const s of v.sizes) opts.push({ key: String(s.height), label: `${s.height}p MP4${tag(s.size)}` });
  opts.push({ key: "audio-mp3", label: `Audio only — MP3${tag(v.mp3Size)}` });
  opts.push({ key: "audio-m4a", label: `Audio only — M4A${tag(v.audioSize)}` });
  sel.replaceChildren(...opts.map((f) => el("option", { value: f.key, text: f.label })));
  // Keep the earlier choice if it's offered; a resolution the video lacks falls to the nearest one below.
  if (opts.some((o) => o.key === want)) sel.value = want;
  else {
    const n = parseInt(want, 10);
    const below = Number.isFinite(n) ? v.sizes.find((s) => s.height <= n) : null;
    sel.value = below ? String(below.height) : "best";
  }
}

async function refreshVideo(gen, urls) {
  { // back to the full list until this page's own qualities are known
    const sel = $("#format"), keep = sel.value;
    sel.replaceChildren(...A.init.formats.map((f) => el("option", { value: f.key, text: f.label })));
    sel.value = keep;
    if (sel.selectedIndex < 0) sel.selectedIndex = 0;
  }
  setName("");
  A.picks = [];
  showPicker();
  if (urls.length > 1) { setInfo([`${urls.length} video links — each title is detected when it downloads.`]); return; }
  setInfo([A.init.ytInstalled ? "Reading the video page…" : "Getting yt-dlp and ffmpeg ready (once, about 17 MB + 100 MB from their official releases), then reading the video page…"]);
  try {
    const v = await invoke("video_probe", { url: urls[0] });
    if (gen !== A.gen) return;
    A.init.ytInstalled = true;
    A.video = v;
    A.probedUrl = urls[0];
    setName(v.title);
    applyVideoFormats(v);
    const lines = [v.title || "(untitled)"];
    buildPicker(v.playlist ? v.entryList || [] : []);
    if (v.playlist) {
      lines.push(`Playlist · ${v.entries} videos — tick “Whole playlist”, then pick which ones below.`);
      $("#playlist").checked = true;
      showPicker();
    } else {
      const bits = [];
      if (v.duration > 0) { const s = Math.round(v.duration); bits.push(s >= 3600 ? `${Math.floor(s / 3600)}:${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}` : `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`); }
      if (v.heights.length) bits.push(`up to ${v.heights[0]}p`);
      if (v.approxSize > 0) bits.push(`~${fmtSize(v.approxSize)} at best quality`);
      if (v.site) bits.push(v.site);
      if (bits.length) lines.push(bits.join(" · "));
    }
    if (v.liveStatus === "is_live") lines.push("🔴 Live now — it records until the stream ends (Settings → Videos: record from the start).");
    if (v.liveStatus === "is_upcoming") lines.push("⏳ Scheduled stream — it hasn't started (Settings → Videos: wait for it to begin).");
    if (A.cls.dups[0]) lines.push(`⚠ Already in your list (${A.cls.dups[0]}).`);
    else if (v.duplicate) lines.push(`⚠ This video is already in your list (${v.duplicate}).`);
    setInfo(lines);
  } catch (e) {
    if (gen === A.gen) setInfo([`yt-dlp couldn't read this page: ${e}`]);
  }
}

// Playlist picker: which entries of a playlist to download (numbered from 1, as yt-dlp counts them).
function buildPicker(entries) {
  A.picks = entries.map((e) => ({ ...e, on: true }));
  const list = $("#pickList");
  list.replaceChildren(...A.picks.map((e) => {
    const box = el("input", { type: "checkbox" });
    box.checked = true;
    box.addEventListener("change", () => { e.on = box.checked; pickCount(); });
    e.box = box;
    const mins = e.duration > 0 ? ` · ${Math.floor(e.duration / 60)}:${String(Math.round(e.duration % 60)).padStart(2, "0")}` : "";
    return el("label", { class: "check", style: { display: "flex" } }, box, el("span", { text: `${e.n}. ${e.title || "(untitled)"}${mins}` }));
  }));
  pickCount();
  showPicker();
}
function showPicker() { $("#playlistPicker").hidden = !(videoMode() && $("#playlist").checked && A.picks?.length); }
function pickCount() {
  const on = (A.picks || []).filter((e) => e.on).length;
  $("#pickCount").textContent = `${on} of ${(A.picks || []).length} videos ticked`;
}
function setPicks(fn) { for (const e of A.picks || []) { e.on = fn(e.n); e.box.checked = e.on; } pickCount(); }
/** "1,3,5-9" for yt-dlp, or "" when everything is ticked. */
function pickedItems() {
  const p = A.picks || [];
  if (!p.length || p.every((e) => e.on)) return "";
  const nums = p.filter((e) => e.on).map((e) => e.n), parts = [];
  for (let i = 0; i < nums.length; i++) {
    let j = i;
    while (j + 1 < nums.length && nums[j + 1] === nums[j] + 1) j++;
    parts.push(j > i ? `${nums[i]}-${nums[j]}` : String(nums[i]));
    i = j;
  }
  return parts.join(",");
}
$("#pickAll").addEventListener("click", () => setPicks(() => true));
$("#pickNone").addEventListener("click", () => setPicks(() => false));
$("#pickRange").addEventListener("keydown", (ev) => {
  if (ev.key !== "Enter") return;
  ev.preventDefault();
  const want = new Set();
  for (const part of ev.target.value.split(",").map((s) => s.trim()).filter(Boolean)) {
    const [a, b] = part.split("-").map((x) => parseInt(x, 10));
    if (!(a > 0)) continue;
    for (let n = a; n <= (b >= a ? b : a); n++) want.add(n);
  }
  setPicks((n) => want.has(n));
});
$("#playlist").addEventListener("change", showPicker);

async function refreshTorrent(gen, urls) {
  $("#torrentPanel").hidden = true;
  if (urls.length > 1) { setName(""); setInfo([`${urls.length} torrents — the saved filename exclusions apply to each torrent.`]); return; }
  const src = urls[0];
  setName(A.cls.magnetName || "");
  setInfo([src.toLowerCase().startsWith("magnet:") ? "Getting the torrent's info from peers… (usually a few seconds)" : "Reading the torrent…"]);
  try {
    const t = await invoke("torrent_info", { source: src, headers: $("#headers").value });
    if (gen !== A.gen) return;
    A.torrent = t;
    A.probedUrl = src;
    setName(t.name);
    A.torrentFiles = t.files.map((f) => ({ ...f, parts: String(f.path).split(/[\\/]+/).filter(Boolean), on: f.wanted !== false }));
    A.openDirs = new Set();
    A.tree = buildTree(A.torrentFiles);
    // Small torrents open fully; big ones show their top level first.
    if (t.files.length <= 300) walkDirs(A.tree, (d) => A.openDirs.add(d.key));
    else A.tree.dirs.forEach((d) => A.openDirs.add(d.key));
    $("#tFilter").value = "";
    $("#torrentPanel").hidden = false;
    drawTorrentFiles();
    growForTorrent();
    const ratio = A.init.seedRatio;
    const warning = await freeWarning(t.size);
    if (gen !== A.gen) return;
    setInfo([
      t.name,
      `Torrent · ${fmtSize(t.size)} · ${t.files.length} file${t.files.length === 1 ? "" : "s"}` + (ratio > 0 ? ` · shares back until ratio ${+ratio.toFixed(2)}` : " · no seeding"),
      t.dup ? `⚠ Already in your list (${t.dup}).` : null,
      warning,
    ]);
  } catch (e) {
    if (gen === A.gen) setInfo([`Couldn't get the torrent's info yet (${e}).`, "You can still add it — FluxDM keeps looking for peers once it starts."]);
  }
}

// ---------------------------------------------------------------- torrent file tree (qBittorrent-style)

/** Folders from the files' paths: { key, name, dirs: [], files: [], size }. */
function buildTree(files) {
  const root = { key: "", name: "", dirs: [], files: [], size: 0, byName: new Map() };
  for (const f of files) {
    let node = root;
    root.size += f.length;
    for (const part of f.parts.slice(0, -1)) {
      let d = node.byName.get(part);
      if (!d) { d = { key: node.key + "/" + part, name: part, dirs: [], files: [], size: 0, byName: new Map() }; node.byName.set(part, d); node.dirs.push(d); }
      d.size += f.length;
      node = d;
    }
    node.files.push(f);
  }
  const cmp = (a, b) => a.localeCompare(b, undefined, { numeric: true });
  const sort = (n) => { n.dirs.sort((a, b) => cmp(a.name, b.name)); n.files.sort((a, b) => cmp(a.path, b.path)); n.dirs.forEach(sort); };
  sort(root);
  return root;
}
function walkDirs(n, fn) { for (const d of n.dirs) { fn(d); walkDirs(d, fn); } }
function filesUnder(n, keep) { const out = n.files.filter(keep); for (const d of n.dirs) out.push(...filesUnder(d, keep)); return out; }

function drawTorrentFiles() {
  const box = $("#torrentFiles");
  const q = $("#tFilter").value.trim().toLowerCase();
  const keep = (f) => !q || f.path.toLowerCase().includes(q);
  const rows = [];
  const pad = (depth) => ({ paddingLeft: 8 + depth * 18 + "px" });
  const fileRow = (f, depth) => {
    const priority = el("select", { class: "tpriority", "aria-label": "Priority for " + f.path });
    priority.replaceChildren(...["Low","Normal","High","Maximum"].map((text,i)=>el("option",{value:i+1,text})));
    priority.value=f.priority || 2;
    priority.addEventListener("click",e=>e.stopPropagation());
    priority.addEventListener("change",()=>{f.priority=+priority.value;});
    const cb = el("input", { type: "checkbox" });
    cb.checked = f.on;
    cb.addEventListener("change", () => { f.on = cb.checked; drawTorrentFiles(); });
    return el("label", { class: "trow" + (f.on ? "" : " off"), title: f.path, style: pad(depth) },
      el("span", { class: "tw" }), cb, el("span", { class: "ti", html: icon("file") }),
      el("span", { class: "tn", text: f.parts[f.parts.length - 1] || f.path }), priority, el("span", { class: "tsize", text: fmtSize(f.length) }));
  };
  const walk = (n, depth) => {
    for (const d of n.dirs) {
      const files = filesUnder(d, keep);
      if (!files.length) continue;
      const open = q ? true : A.openDirs.has(d.key);
      const on = files.filter((f) => f.on).length;
      const cb = el("input", { type: "checkbox" });
      cb.checked = on === files.length;
      cb.indeterminate = on > 0 && on < files.length;
      cb.addEventListener("change", () => { files.forEach((f) => (f.on = cb.checked)); drawTorrentFiles(); });
      const tw = el("span", { class: "tw", html: icon("chev") });
      tw.addEventListener("click", (e) => { e.preventDefault(); if (q) return; if (A.openDirs.has(d.key)) A.openDirs.delete(d.key); else A.openDirs.add(d.key); drawTorrentFiles(); });
      const row = el("label", { class: "trow dir" + (open ? "" : " closed") + (on ? "" : " off"), title: d.key.slice(1), style: pad(depth) },
        tw, cb, el("span", { class: "ti", html: icon("folder") }), el("span", { class: "tn", text: d.name }), el("span", { class: "tsize", text: fmtSize(d.size) }));
      row.addEventListener("dblclick", (e) => { if (e.target !== cb) tw.click(); });
      rows.push(row);
      if (open) walk(d, depth + 1);
    }
    for (const f of n.files) if (keep(f)) rows.push(fileRow(f, depth));
  };
  if (A.tree) walk(A.tree, 0);
  const top = box.scrollTop;
  box.replaceChildren(...(rows.length ? rows : [el("div", { class: "tempty", text: q ? "No file matches the filter." : "This torrent has no files." })]));
  box.scrollTop = top;
  torrentSelection();
}
function torrentSelection() {
  updateFreeSpace();
  const on = A.torrentFiles.filter((f) => f.on);
  $("#torrentSel").textContent = `${on.length} of ${A.torrentFiles.length} file${A.torrentFiles.length === 1 ? "" : "s"} · ${fmtSize(on.reduce((a, f) => a + f.length, 0))}`;
}
/** All / None act on the files the filter shows. */
function setShown(on) {
  const q = $("#tFilter").value.trim().toLowerCase();
  A.torrentFiles.forEach((f) => { if (!q || f.path.toLowerCase().includes(q)) f.on = on; });
  drawTorrentFiles();
}

/** A torrent's file list is the main part of the window: give it room once (never shrinks the window). */
async function growForTorrent() {
  if (A.grown) return;
  A.grown = true;
  try {
    const LS = (T.dpi && T.dpi.LogicalSize) || T.window.LogicalSize;
    const w = Math.min(screen.availWidth - 40, 760), h = Math.min(screen.availHeight - 40, 860);
    if (innerWidth >= w && innerHeight >= h) return;
    await win.setSize(new LS(Math.max(innerWidth, w), Math.max(innerHeight, h)));
    await win.center();
  } catch { /* keep the size it has */ }
}

let submitting = false;
async function submit(start) {
  if (submitting) return; // a double click or Enter + click must not add everything twice
  submitting = true;
  try { await doSubmit(start); } finally { submitting = false; }
}

async function doSubmit(start) {
  const cls = await invoke("classify", { text: urlsBox.value });
  if (!cls.urls.length) return;
  const limit = parseSpeed($("#limit").value);
  if (limit == null) return alertBox("Add download", "Speed limit: type a number of KB/s, or something like 500k or 2 MB/s (0 = unlimited).");
  const single = cls.urls.length === 1;
  const mode = cls.torrent ? "torrent" : videoMode() ? "video" : "file";
  const up = parseSpeed($("#upLimit").value, true), seed = parseSpeed($("#seedLimit").value, true);
  if (mode === "torrent" && (up == null || seed == null))
    return alertBox("Add download", "Upload limit: type a number of KB/s, something like 20k or 1 MB/s, “No upload”, or 0 for only the limits in Settings.");
  const tInfo = mode === "torrent" && single && A.torrent && A.probedUrl === cls.urls[0];
  const skipped = tInfo ? A.torrentFiles.filter((f) => !f.on).map((f) => f.index) : null;
  if (tInfo && skipped.length === A.torrentFiles.length) return alertBox("Add download", "Tick at least one file of the torrent to download.");
  if (mode === "video" && single && $("#playlist").checked && A.picks?.length && !A.picks.some((e) => e.on))
    return alertBox("Add download", "Tick at least one video of the playlist.");
  const probed = mode === "file" && single && A.probe && A.probedUrl === cls.urls[0];
  const req = {
    urls: cls.urls, mode,
    name: nameBox.value, nameEdited: A.nameEdited,
    folder: folderBox.value, folderEdited: A.folderEdited,
    connections: Math.max(1,Math.min(64,+$("#conn").value || 1)), limitKbps: limit,
    uploadLimitKbps: mode === "torrent" ? up : 0, seedingLimitKbps: mode === "torrent" ? seed : 0,
    queue: $("#queue").value,
    headers: $("#headers").value,
    hash: $("#hash").value,
    mirrors: $("#mirrors").value.split("\n").map((l) => l.trim()).filter((l) => /^https?:\/\//i.test(l)),
    videoFormat: $("#format").value,
    playlist: $("#playlist").checked,
    videoTitle: mode === "video" && single && A.video && A.probedUrl === cls.urls[0] ? A.video.title : null,
    videoItems: mode === "video" && single && $("#playlist").checked && A.probedUrl === cls.urls[0] ? pickedItems() : "",
    videoSection: mode === "video" && single ? $("#section").value.trim() : "",
    videoId: mode === "video" && single && A.video && A.probedUrl === cls.urls[0] ? A.video.id || "" : "",
    probeTotal: probed ? A.probe.total : null,
    probeResumable: probed ? A.probe.resumable : null,
    torrentName: tInfo ? A.torrent.name : null,
    torrentHash: tInfo ? A.torrent.infoHash : null,
    torrentFile: tInfo ? A.torrent.cachedFile : null,
    skipped,
    filePriorities: tInfo ? Object.fromEntries(A.torrentFiles.filter(f=>f.on).map(f=>[f.index,f.priority || 2])) : {},
    wantedSize: tInfo ? A.torrentFiles.filter((f) => f.on).reduce((a, f) => a + f.length, 0) : null,
    openWhenDone: $("#openDone").checked,
    start,
  };
  try { await invoke("add_submit", { req }); }
  catch (e) { alertBox("Add download", String(e)); }
}

urlsBox.addEventListener("input", () => { autoGrow(); refreshSoon(); });
// A pasted link shows from its start (what it is), not its end (tracker lists, tokens).
urlsBox.addEventListener("paste", () => setTimeout(toFirstLine, 0));
$("#headers").addEventListener("input", refreshSoon);
nameBox.addEventListener("input", () => { A.nameEdited = true; updateAutoFolder(); renderDetected(); lookupArt(); });
folderBox.addEventListener("input", () => { A.folderEdited = true; updateFreeSpace(); });
$("#conn").addEventListener("input", (e) => ($("#connVal").textContent = e.target.value));
$("#video").addEventListener("change", () => { A.videoTouched = true; applyModes(); refresh(); });
$("#tAll").onclick = () => setShown(true);
$("#tNone").onclick = () => setShown(false);
$("#tFilter").addEventListener("input", drawTorrentFiles);
$("#expand").onclick = () => { setExpanded(!A.expanded); urlsBox.focus(); };
$("#copyLink").onclick = async () => {
  const text = urlsBox.value.trim();
  if (!text) return;
  await invoke("copy_text", { text });
  const b = $("#copyLink");
  b.classList.add("done");
  b.innerHTML = icon("check");
  setTimeout(() => { b.classList.remove("done"); b.innerHTML = icon("copy"); }, 1200);
};
$("#browse").onclick = async () => {
  const d = await T.dialog.open({ directory: true, title: "Save downloads to", defaultPath: folderBox.value });
  if (d) { folderBox.value = d; A.folderEdited = true; updateFreeSpace(); }
};
$("#openTorrent").onclick = async () => {
  const f = await T.dialog.open({ title: "Add .torrent files", multiple: true, filters: [{ name: "Torrent files", extensions: ["torrent"] }] });
  if (f) append((Array.isArray(f) ? f : [f]).join("\n"));
};
$("#cancel").onclick = () => win.close();
$("#later").onclick = () => submit(false);
$("#start").onclick = () => submit(true);
document.addEventListener("keydown", (e) => {
  if ($(".modal-back")) return;
  if (e.key === "Escape") win.close();
  // The one-line link box: Shift+Enter starts another line (and opens the box), Enter starts the download.
  if (e.key === "Enter" && e.shiftKey && e.target === urlsBox && !A.expanded) { setExpanded(true); return; }
  const oneLine = e.target === urlsBox && !A.expanded && !e.shiftKey;
  if (e.key === "Enter" && (e.ctrlKey || oneLine || e.target.tagName !== "TEXTAREA") && e.target.tagName !== "BUTTON") { e.preventDefault(); if (!$("#start").disabled) submit(true); }
});

// Drops onto this window: .torrent files and .txt lists of links.
addEventListener("dragover", (e) => { e.preventDefault(); e.dataTransfer.dropEffect = "copy"; });
addEventListener("drop", async (e) => {
  e.preventDefault();
  const files = [...e.dataTransfer.files];
  for (const f of files) {
    const n = f.name.toLowerCase();
    if (n.endsWith(".torrent")) { try { await invoke("dropped_torrent", { bytes: [...new Uint8Array(await f.arrayBuffer())] }); } catch (err) { alertBox("Torrent", String(err)); } }
    else if (n.endsWith(".txt")) append(await f.text());
  }
  if (!files.length) { const t = e.dataTransfer.getData("text/plain"); if (t) append(t); }
});

let freeGeneration = 0, artTimer = 0;
async function updateFreeSpace() {
  if (!A.init) return;
  const gen = ++freeGeneration, folder = folderBox.value.trim() || A.init.defaultFolder;
  const size = torrentMode() && A.torrent ? A.torrentFiles.filter(f=>f.on).reduce((n,f)=>n+f.length,0) : A.probe?.total || A.video?.approxSize || 0;
  try {
    const free = await invoke("free_space", { folder });
    if (gen !== freeGeneration) return;
    const low = free != null && size > 0 && free < size;
    $("#freeSpace").classList.toggle("warn",low);
    $("#freeSpace").textContent = free == null ? "Free space unavailable" : fmtSize(free)+" free"+(size > 0 ? low ? " \u00b7 not enough space" : " \u00b7 enough space" : "");
  } catch { if(gen===freeGeneration) $("#freeSpace").textContent="Free space unavailable"; }
}
function renderDetected() {
  const name = nameBox.value || A.cls?.names?.[0] || "New download";
  const type = A.art?.game ? "game" : torrentMode() ? "torrent" : videoMode() ? "video" : /\.(gguf|safetensors|ckpt)$/i.test(name) ? "ai" : /\.(exe|msi|msix)$/i.test(name) ? "program" : /\.(zip|7z|rar)$/i.test(name) ? "archive" : "file";
  const glyph = ({game:"game",torrent:"magnet",video:"video",ai:"ai",program:"app",archive:"zip"})[type] || "file";
  $("#detected").className="type-"+type;
  $("#detectedTitle").textContent=A.art?.title || (A.cls?.urls.length > 1 ? A.cls.urls.length+" downloads" : name);
  $("#detectedIcon").innerHTML=A.art?.icon ? '<img alt="" src="'+esc(A.art.icon)+'">' : icon(glyph);
  $("#typeWatermark").innerHTML="";
  const artUrl=setCardArt($("#detectedArt"),A.art?.banner,A.art?.icon,glyph,A.art?.artColor);
  applySurfaceArt(artUrl,A.settings,$("#windowArt"));
  const size=A.torrent?.size || A.probe?.total || A.video?.approxSize || 0;
  $("#detectedSize").textContent=[name, size>0 ? fmtSize(size) : "Size unknown", A.torrent ? A.torrent.files.length+" files" : ""].filter(Boolean).join(" \u00b7 ");
  let host=""; try { host=new URL(A.cls?.urls[0]).hostname; } catch {}
  const chips=[type.charAt(0).toUpperCase()+type.slice(1),host,A.probe?.resumable ? "Resumable" : "",A.video?.playlist ? "Playlist" : ""];
  $("#chips").replaceChildren(...chips.filter(Boolean).map(text=>el("span",{text})));
  $("#headerNote").textContent=A.torrent ? "Torrent details loaded" : A.probe ? "Link checked \u00b7 "+(A.probe.resumable ? "resumable" : "single connection") : videoMode() ? "Video / audio" : "Add download links";
  updateFreeSpace();
}
function lookupArt() {
  clearTimeout(artTimer);
  const name=nameBox.value.trim(), gen=A.gen;
  if(A.art && A.artName!==name) { A.art=null; renderDetected(); applySurfaceArt(null,A.settings,$("#windowArt")); }
  if (!name || A.cls?.urls.length !== 1 || videoMode()) return;
  artTimer=setTimeout(async()=>{
    try { const art=await invoke("add_art",{name,url:A.cls.urls[0],headers:$("#headers").value});
      if(gen!==A.gen || name!==nameBox.value.trim()) return;
      A.art=art; A.artName=name; renderDetected(); applySurfaceArt(art?.banner || art?.icon || fallbackArt("file"),A.settings,$("#windowArt"));
    } catch {}
  },500);
}

listen("appearance-changed",st=>{ A.settings=st; applySurfaceArt(A.art?.banner || A.art?.icon || fallbackArt("file"),st,$("#windowArt")); });
