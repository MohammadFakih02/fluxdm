// FluxDM main window: the download list, sidebar, details pane, and the Settings / Scheduler / Stats dialogs.
"use strict";

const COLUMNS = [
  { k: "#", w: 42, help: "The row number", num: true },
  { k: "File name", w: 320, sort: (r) => r.name.toLowerCase() },
  { k: "Size", w: 90, help: "How big the download is", sort: (r) => r.total, num: true },
  { k: "Progress", w: 150, help: "How much of it is downloaded so far", sort: (r) => r.progress },
  { k: "Status", w: 170, help: "Downloading, paused, finished, or what went wrong", sort: (r) => r.seeding ? "Seeding" : r.status },
  { k: "Seeds", w: 80, help: "Torrents only: people with the complete file you're connected to (total in brackets)", sort: (r) => parseInt(r.seeds) || 0, num: true },
  { k: "Peers", w: 80, help: "Torrents only: other people still downloading it, connected (total in brackets)", sort: (r) => parseInt(r.peers) || 0, num: true },
  { k: "Speed", w: 95, help: "How fast it's downloading right now", sort: (r) => r.speed, num: true },
  { k: "Up Speed", w: 95, help: "Torrents only: how fast you're sharing it back", sort: (r) => r.upSpeed, num: true },
  { k: "Time left", w: 85, help: "Roughly how long until it's finished", sort: (r) => (r.eta ? (r.total - r.downloaded) / Math.max(r.speed, 1) : Infinity), num: true },
  { k: "Availability", w: 85, help: "Torrents only: how many complete copies of what you still need the people you're connected to have. Below 1.00 some piece isn't on any of them, so it can't finish until someone with it turns up", sort: (r) => r.availabilityNum, num: true },
  { k: "Ratio", w: 60, help: "Torrents only: how much you've shared compared with what you downloaded", sort: (r) => (r.downloaded > 0 ? r.uploaded / r.downloaded : 0), num: true },
  { k: "Category", w: 95, help: "What kind of file it is — video, program, archive…", sort: (r) => r.category },
  { k: "Tags", w: 110, help: "Your own labels (right-click a download → Edit tags)", sort: (r) => r.tags.join(",") },
  { k: "Conn.", w: 62, help: "How many connections this download uses", sort: (r) => r.connections, num: true },
  { k: "Queue", w: 105, help: "Which queue it belongs to", sort: (r) => r.queue },
  { k: "Time Active", w: 95, help: "How long it has spent downloading or sharing", sort: (r) => r.activeSeconds, num: true },
  { k: "Last Activity", w: 100, help: "When data last moved" },
  { k: "Added", w: 105, help: "When you added it", sort: (r) => r.addedTs },
];
const SIMPLE = ["File name", "Size", "Progress", "Status", "Speed", "Time left"];
const TORRENT_SET = [...SIMPLE, "Seeds", "Peers", "Up Speed", "Ratio"];

const S = {
  settings: null,
  categories: [],
  rows: new Map(), // id -> row json
  order: [], // backend order
  summary: null,
  sel: new Set(),
  cur: null, // focused id
  anchor: null,
  nav: "all",
  search: "",
  sortKey: "Added",
  sortDir: -1,
  groupBy: "None",
  cols: [], // [{k, w, visible}] in display order
  tab: "general",
  details: null,
  update: null,
};

const trs = new Map(); // id -> <tr>

async function showFileConflict(p) {
  const policy = await new Promise(resolve => modal({ title: "File already exists", body: el("p", { text: p.name + " already exists in the destination. Choose what to do." }), buttons: [
    { label: "Skip", value: "Skip" }, { label: "Add a number", value: "Number", primary: true }, { label: "Overwrite", value: "Overwrite", danger: true }
  ], onClose: resolve }));
  if (!policy) return;
  await invoke("set_items", { ids: [p.id], patch: { existingFilePolicy: policy } });
  if (policy !== "Skip") await invoke("start", { ids: [p.id] });
}

// ---------------------------------------------------------------- start-up

(async function init() {
  $$("[data-icon]").forEach((e) => (e.innerHTML = icon(e.dataset.icon)));
  const d = await invoke("init");
  S.settings = d.settings;
  S.categories = d.categories;
  applyAppearance(S.settings);
  updateWindowArt();
  loadLayout();
  buildHead();
  wire();
  await listen("list", onList);
  await listen("settings-changed", async () => {
    const g = await invoke("get_settings");
    S.settings = g.settings;
    applyAppearance(S.settings);
    artKey = ""; updateWindowArt(); rebuildAllRows(); refreshFreeSpace();
    drawSeg(); drawGraph();
    showLimit();
  });
  await listen("appearance-changed", (settings) => { S.settings = { ...S.settings, ...settings }; artKey = ""; updateWindowArt(); rebuildAllRows(); });
  await listen("countdown", showCountdown);
  await listen("file-conflict", showFileConflict);
  await listen("torrent-duplicate", (p) => selectOnly(p.id));
  await listen("offer-firewall", offerFirewall);
  await listen("update-available", (rel) => { S.update = rel; $("#updateBadge").hidden = false; $("#btnUpdate").textContent = `Update to ${rel.version}`; });
  await listen("update-progress", (p) => { const b = $("#updProg"); if (b) b.style.width = p + "%"; });
  showLimit();
  await invoke("ready");
  setTimeout(askStartWithWindows, 4000);
  setInterval(() => refreshDetails(true), 1000);
  refreshFreeSpace(); setInterval(refreshFreeSpace, 30000);
})();

// ---------------------------------------------------------------- list updates

function onList(p) {
  for (const r of p.changed) S.rows.set(r.id, r);
  for (const id of p.removed) {
    S.rows.delete(id);
    S.sel.delete(id);
    const tr = trs.get(id);
    if (tr) { tr.remove(); trs.delete(id); }
    if (S.cur === id) S.cur = null;
  }
  if (p.order) S.order = p.order;
  S.summary = p.summary;
  for (const r of p.changed) renderRow(r);
  if (p.order || p.removed.length || p.changed.length || !S.arranged) { arrange(); S.arranged = true; }
  renderNav();
  renderStatus();
  updateResumeAction();
  if (S.cur && p.changed.some((r) => r.id === S.cur)) renderDetails();
  if (!S.cur && !$("#detailsPane").hidden) renderDetails();
}

function visibleCols() { return S.cols.filter((c) => c.visible); }
function colDef(k) { return COLUMNS.find((c) => c.k === k); }

function statusClass(r) { return "st-" + r.status; }
function statusLabel(r) {
  return r.seeding ? "Seeding" : r.torrent && r.status === "Verifying" ? r.statusText : r.status === "Completed" ? "Done" : r.status;
}
function canPause(r) { return r.running || r.seeding || r.status === "Queued" || r.torrent && r.status === "Verifying"; }

function cellHtml(k, r) {
  switch (k) {
    case "#": return "";
    case "File name": {
      const ic = r.icon ? `<img src="${esc(r.icon)}" data-cat="${esc(r.category)}">` : icon(CATEGORY_ICON[r.category] || "file");
      return `<div class="cell-name" title="${esc(r.path)}"><span class="ic">${ic}</span><span class="nm">${esc(r.title || r.name)}</span></div>`;
    }
    case "Size": return esc(r.sizeText);
    case "Progress": {
      const cls = r.status === "Completed" ? "done" : r.status === "Error" ? "err" : r.running || r.status === "Queued" ? "" : "paused";
      return `<div class="cell-prog"><span class="pt">${esc(r.progressText)}</span><div class="bar ${cls}"><i style="width:${r.progress}%"></i></div></div>`;
    }
    case "Status": return `<span class="pill ${r.seeding ? "st-Seeding" : statusClass(r)}" title="${esc(r.statusText)}">${esc(r.statusText)}</span>`;
    case "Seeds": return esc(r.seeds);
    case "Peers": return esc(r.peers);
    case "Speed": return esc(r.speedText);
    case "Up Speed": return esc(r.upSpeedText);
    case "Time left": return esc(r.eta);
    case "Ratio": return esc(r.ratio);
    case "Availability": return r.availability !== "" && +r.availability < 1 ? `<span class="st-Error" title="Some pieces you still need aren't on any connected peer">${esc(r.availability)}</span>` : esc(r.availability);
    case "Category": return esc(r.category);
    case "Tags": return esc(r.tags.join(", "));
    case "Conn.": return esc(r.conn);
    case "Queue": return esc(r.queue);
    case "Time Active": return esc(r.timeActive);
    case "Last Activity": return esc(r.lastActivity);
    case "Added": return esc(r.added);
  }
  return "";
}

function renderRow(r) {
  let tr = trs.get(r.id);
  if (!tr) {
    tr = el("tr", { class: "r" });
    tr.dataset.id = r.id;
    trs.set(r.id, tr);
    buildCells(tr);
  }
  if (!S.tableView) {
    const h = cardHtml(r);
    if (tr._h !== h) { tr.innerHTML = `<td colspan="${visibleCols().length + 1}">${h}</td>`; tr._h = h; }
    tr.querySelector(".download-card")?.classList.toggle("has-art", cardShowsArt(r));
    tr.classList.toggle("sel", S.sel.has(r.id));
    tr.classList.toggle("cur", S.cur === r.id);
    return;
  }
  const cells = tr.children;
  visibleCols().forEach((c, i) => {
    const td = cells[i];
    if (c.k === "#") return;
    const h = cellHtml(c.k, r);
    if (td._h !== h) { td.innerHTML = h; td._h = h; }
  });
  tr.classList.toggle("sel", S.sel.has(r.id));
  tr.classList.toggle("cur", S.cur === r.id);
}

function buildCells(tr) {
  tr._h = null;
  tr.replaceChildren();
  for (const c of visibleCols()) tr.append(el("td", { class: (colDef(c.k).num ? "num " : "") + "c-" + c.k.replace(/\W/g, "") }));
  tr.append(el("td", { class: "c-filler" }));
}

function rebuildAllRows() {
  for (const [id, tr] of trs) { buildCells(tr); const r = S.rows.get(id); if (r) renderRow(r); }
  arrange();
}

function matchesNav(r, nav = S.nav) {
  if (nav === "all") return true;
  if (nav === "games") return r.game ?? !!(r.banner || r.cover);
  if (nav === "active") return r.seeding || ["Connecting", "Downloading", "Verifying"].includes(r.status);
  if (nav === "paused") return ["Paused", "Stopped"].includes(r.status);
  if (nav === "queued") return r.status === "Queued";
  if (nav === "unfinished") return r.status !== "Completed";
  if (nav === "finished") return r.status === "Completed";
  if (nav.startsWith("cat:")) return r.category === nav.slice(4);
  if (nav.startsWith("q:")) return r.queue === nav.slice(2);
  if (nav.startsWith("tag:")) return r.tags.some((t) => t.toLowerCase() === nav.slice(4).toLowerCase());
  return true;
}

function matches(r) {
  if (!matchesNav(r)) return false;
  const q = S.search.trim().toLowerCase();
  return !q || r.name.toLowerCase().includes(q) || r.url.toLowerCase().includes(q);
}

const GROUP_OPTIONS = ["None", "Date added", "Status", "Type/category", "Site", "Queue"];
function groupDate(ts) { const d = new Date((ts || 0) * 1000); d.setHours(0,0,0,0); return d.getTime(); }
function groupLabel(r) {
  switch (S.groupBy) {
    case "Date added": return dayLabel(r.addedTs);
    case "Status": return r.seeding ? "Seeding" : r.status;
    case "Type/category": return r.category || "Other";
    case "Site":
      if (r.torrent) return "Torrents";
      try { return new URL(r.url).hostname || "Other"; } catch { return "Other"; }
    case "Queue": return r.queue || "No queue";
    default: return "";
  }
}
function sortedIds() {
  let ids = S.order.filter((id) => S.rows.has(id));
  if (S.sortKey) {
    const def = colDef(S.sortKey);
    const f = def?.sort || ((r) => cellHtml(S.sortKey, r));
    const idx = new Map(ids.map((id, i) => [id, i]));
    ids.sort((a, b) => {
      const x = f(S.rows.get(a)), y = f(S.rows.get(b));
      return (x < y ? -1 : x > y ? 1 : 0) * S.sortDir || idx.get(a) - idx.get(b);
    });
  }
  if (S.groupBy !== "None") {
    ids.sort((a, b) => S.groupBy === "Date added"
      ? groupDate(S.rows.get(b).addedTs) - groupDate(S.rows.get(a).addedTs)
      : groupLabel(S.rows.get(a)).localeCompare(groupLabel(S.rows.get(b))));
  }
  return ids;
}

// Puts rows in order, hides filtered ones and renumbers "#".
function arrange() {
  const tbody = $("#rows");
  $$(".day-group", tbody).forEach((e) => e.remove());
  const ids = sortedIds();
  let day = null;
  let n = 0, prev = null;
  const numIdx = visibleCols().findIndex((c) => c.k === "#");
  for (const id of ids) {
    const tr = trs.get(id);
    if (!tr) continue;
    const show = matches(S.rows.get(id));
    tr.classList.toggle("hide", !show);
    if (show) {
      n++;
      if (S.tableView && numIdx >= 0 && tr.children[numIdx].textContent !== String(n)) tr.children[numIdx].textContent = n;
    }
    if (show && S.groupBy !== "None") {
      const label = groupLabel(S.rows.get(id));
      if (day !== label) {
        const group = el("tr", { class: "day-group" }, el("td", { colspan: visibleCols().length + 1, text: label }));
        tbody.insertBefore(group, prev ? prev.nextSibling : tbody.firstChild);
        prev = group; day = label;
      }
    }
    const want = prev ? prev.nextSibling : tbody.firstChild;
    if (want !== tr) tbody.insertBefore(tr, want);
    prev = tr;
  }
  $("#listCount").textContent = `${n} downloads`;
  $("#emptyList").hidden = n > 0;
  if (!n) {
    const filtered = S.rows.size > 0 || !!S.search.trim() || S.nav !== "all";
    $("#emptyTitle").textContent = filtered ? "No matches" : "No downloads yet";
    $("#emptyHint").textContent = filtered ? "Try clearing your search or filter." : "Add a link to start downloading.";
    $("#emptyAction").textContent = filtered ? "Clear search/filter" : "Add download";
    $("#emptyAction").onclick = filtered ? () => {
      S.search = ""; S.nav = "all"; $("#searchBox").value = ""; renderNav(); arrange();
    } : () => invoke("open_add", { queue: navQueue() });
  }
  for (const id of [...S.sel]) if (!S.rows.has(id)) S.sel.delete(id);
}

function visibleIds() { return sortedIds().filter((id) => matches(S.rows.get(id))); }

// ---------------------------------------------------------------- columns & layout

function setRailWidth(width) {
  const max = Math.min(240, Math.max(150, innerWidth - 520));
  S.railWidth = Math.max(62, Math.min(max, Number(width) || 62));
  $("#sidebar").style.width = S.railWidth + "px";
  $("#sidebar").classList.toggle("expanded", S.railWidth >= 150);
}

function loadLayout() {
  const st = S.settings;
  S.sortKey = COLUMNS.some((c) => c.k === st.DownloadSortKey && c.sort) ? st.DownloadSortKey : "Added";
  S.sortDir = st.DownloadSortDirection === 1 ? 1 : -1;
  S.groupBy = GROUP_OPTIONS.includes(st.DownloadGroupBy) ? st.DownloadGroupBy : "None";
  S.tableView = st.DownloadTableView === true;
  document.body.classList.toggle("table-view", S.tableView);
  $("#downloadColumn").style.width = Math.max(280, Math.min(600, st.DownloadListWidth || 360)) + "px";
  setRailWidth(st.RailWidth >= 150 ? st.RailWidth : 62);
  const saved = st.Columns || {};
  S.cols = COLUMNS.map((c, i) => {
    const s = saved["Downloads:" + c.k];
    return { k: c.k, w: s && s.Width > 0 ? s.Width : c.w, visible: s ? s.Visible : SIMPLE.includes(c.k), order: s ? s.Order : i };
  });
  S.cols.sort((a, b) => a.order - b.order);
  if ((st.ColumnsLayoutVersion || 0) < 2) S.cols.forEach((c) => (c.visible = SIMPLE.includes(c.k)));
  S.cols.find((c) => c.k === "File name").visible = true;
  const split = st.DetailsSplit;
  if (split > 0 && split < 1) { $("#genLeft").style.flex = `${split} 1 0`; $("#genRight").style.flex = `${1 - split} 1 0`; }
  for (const k of ["Trackers", "Peers", "Files"]) S["w" + k] = {};
  for (const [key, v] of Object.entries(saved)) {
    const [grid, name] = key.split(":");
    if (S["w" + grid] && v.Width > 0) S["w" + grid][name] = v.Width;
  }
}

let layoutTimer = 0;
function saveLayout() {
  clearTimeout(layoutTimer);
  layoutTimer = setTimeout(() => {
    const columns = {};
    S.cols.forEach((c, i) => (columns["Downloads:" + c.k] = { Width: Math.round(c.w), Order: i, Visible: c.visible }));
    for (const g of ["Trackers", "Peers", "Files"]) for (const [n, w] of Object.entries(S["w" + g] || {})) columns[`${g}:${n}`] = { Width: w, Order: 0, Visible: true };
    Object.assign(S.settings, { DownloadSortKey: S.sortKey, DownloadSortDirection: S.sortDir, DownloadGroupBy: S.groupBy, DownloadTableView: !!S.tableView, RailWidth: S.railWidth || 62, DownloadListWidth: $("#downloadColumn").getBoundingClientRect().width });
    invoke("save_layout", {
      columns,
      layout: { railWidth: S.railWidth || 62, listWidth: $("#downloadColumn").getBoundingClientRect().width,
      sortKey: S.sortKey || "Added", sortDirection: S.sortDir, groupBy: S.groupBy, tableView: !!S.tableView },
      sidebar: $("#sidebar").getBoundingClientRect().width,
      details: $("#details").style.height ? parseFloat($("#details").style.height) : (S.settings?.DetailsHeight ?? 270),
      split: parseFloat($("#genLeft").style.flexGrow) || S.settings?.DetailsSplit || 0.5,
    });
  }, 400);
}

function updateSortControls() {
  $("#sortSelect").value = S.sortKey;
  $("#groupSelect").value = S.groupBy;
  $("#sortDirection").innerHTML = icon(S.sortDir === 1 ? "arrowUp" : "arrowDown");
  $("#sortDirection").setAttribute("aria-label", S.sortDir === 1 ? "Ascending; switch to descending" : "Descending; switch to ascending");
}

function buildHead() {
  updateSortControls();
  const head = $("#head");
  head.replaceChildren();
  for (const c of visibleCols()) {
    const th = el("th", { title: colDef(c.k).help || null, draggable: "true", style: { width: c.w + "px" } });
    th.dataset.k = c.k;
    th.append(c.k === "#" ? "#" : c.k);
    if (S.sortKey === c.k) th.append(el("span", { class: "sort", html: icon(S.sortDir > 0 ? "arrowUp" : "arrowDown") }));
    const grip = el("span", { class: "grip" });
    grip.addEventListener("mousedown", (e) => startColResize(e, c, th));
    grip.addEventListener("click", (e) => e.stopPropagation());
    th.append(grip);
    th.addEventListener("click", () => {
      if (c.k === "#") { S.sortKey = "Added"; S.sortDir = -1; }
      else if (S.sortKey === c.k) { if (S.sortDir < 0) S.sortKey = "Added"; else S.sortDir = -1; }
      else { S.sortKey = c.k; S.sortDir = 1; }
      buildHead();
      arrange();
      $("#sortSelect").value = S.sortKey || "Added";
      saveLayout();
    });
    th.addEventListener("contextmenu", (e) => { e.preventDefault(); columnsMenu(e.clientX, e.clientY); });
    th.addEventListener("dragstart", (e) => { e.dataTransfer.setData("text/fluxcol", c.k); e.dataTransfer.effectAllowed = "move"; });
    th.addEventListener("dragover", (e) => { if (e.dataTransfer.types.includes("text/fluxcol")) { e.preventDefault(); th.classList.add("drag-over"); } });
    th.addEventListener("dragleave", () => th.classList.remove("drag-over"));
    th.addEventListener("drop", (e) => {
      th.classList.remove("drag-over");
      const k = e.dataTransfer.getData("text/fluxcol");
      if (!k || k === c.k) return;
      e.preventDefault();
      e.stopPropagation();
      const from = S.cols.findIndex((x) => x.k === k);
      const moved = S.cols.splice(from, 1)[0];
      S.cols.splice(S.cols.findIndex((x) => x.k === c.k), 0, moved);
      buildHead();
      rebuildAllRows();
      saveLayout();
    });
    head.append(th);
  }
  head.append(el("th", { class: "c-filler" }));
  // Columns keep their widths; the filler takes what's left, and the list scrolls sideways only when needed.
  $("#list").style.minWidth = S.tableView ? visibleCols().reduce((a, c) => a + c.w, 0) + "px" : "0";
}

function startColResize(e, c, th) {
  e.preventDefault();
  e.stopPropagation();
  const x0 = e.clientX, w0 = c.w;
  const move = (ev) => {
    c.w = Math.max(8, w0 + ev.clientX - x0);
    th.style.width = c.w + "px";
    $("#list").style.minWidth = visibleCols().reduce((a, x) => a + x.w, 0) + "px";
  };
  const up = () => { removeEventListener("mousemove", move); removeEventListener("mouseup", up); onEnd(); saveLayout(); };
  addEventListener("mousemove", move);
  addEventListener("mouseup", up);
}

function columnsMenu(x, y) {
  const items = [{ header: "Tick the columns you want to see" }];
  for (const c of S.cols) {
    if (c.k === "File name") continue;
    items.push({
      label: c.k === "#" ? "# (row number)" : c.k, checked: c.visible, title: colDef(c.k).help,
      click: () => { c.visible = !c.visible; buildHead(); rebuildAllRows(); saveLayout(); },
    });
  }
  const preset = (names) => () => { S.cols.forEach((c) => (c.visible = names.includes(c.k))); buildHead(); rebuildAllRows(); saveLayout(); };
  items.push("-",
    { label: "Simple (recommended)", title: "Just the essentials: name, size, progress, status, speed and time left", click: preset(SIMPLE) },
    { label: "Simple + torrent details", title: "Adds seeds, peers, upload speed and ratio", click: preset(TORRENT_SET) },
    { label: "Show everything", click: preset(COLUMNS.map((c) => c.k)) },
    "-", { header: "Drag a column's edge to resize it, or its title to move it" });
  showMenu(items, x, y);
}

// Panel splitters: sidebar width, details height, details left/right.
function splitter(handle, onMove, onEnd = () => {}) {
  let pointer = null;
  const finish = e => {
    if (pointer === null || e.pointerId !== pointer) return;
    const id = pointer; pointer = null;
    handle.classList.remove("drag");
    if (handle.hasPointerCapture(id)) handle.releasePointerCapture(id);
    onEnd(); saveLayout();
  };
  handle.addEventListener("pointerdown", e => {
    if (e.button !== 0 || pointer !== null) return;
    e.preventDefault();
    handle.setPointerCapture(e.pointerId);
    pointer = e.pointerId;
    handle.classList.add("drag");
  });
  handle.addEventListener("pointermove", e => { if (e.pointerId === pointer) onMove(e); });
  for (const event of ["pointerup", "pointercancel", "lostpointercapture"]) handle.addEventListener(event, finish);
}

// ---------------------------------------------------------------- sidebar

function renderNav() {
  const rows = [...S.rows.values()];
  const count = (key) => rows.filter((r) => matchesNav(r, key)).length;
  const items = [
    ["all", "All downloads", "all"], ["active", "Active", "down"], ["paused", "Paused", "pause"], ["finished", "Finished", "check"], ["queued", "Queued", "clock"], ["unfinished", "Unfinished", "file"],
    { head: "CATEGORIES" }, ["games", "Games", "game"],
    ...[...S.categories].sort((a, b) => { const order = ["AI Models", "Video", "Programs", "Torrents", "Compressed"]; const rank = (c) => order.includes(c) ? order.indexOf(c) : order.length + S.categories.indexOf(c); return rank(a) - rank(b); }).map((c) => ["cat:" + c, c, CATEGORY_ICON[c] || "file"]),
    { head: "QUEUES" },
    ...(S.summary?.queues || []).map((q) => ["q:" + q.name, q.name, "queue", q.running]),
  ];
  const tags = [...new Set(rows.flatMap((r) => r.tags.map((t) => t.trim())).filter(Boolean))].sort((a, b) => a.localeCompare(b));
  const seen = new Set();
  const uniqTags = tags.filter((t) => (seen.has(t.toLowerCase()) ? false : seen.add(t.toLowerCase())));
  if (uniqTags.length) items.push({ head: "TAGS" }, ...uniqTags.map((t) => ["tag:" + t, t, "tag"]));
  if (!items.some((i) => Array.isArray(i) && i[0] === S.nav)) S.nav = "all";

  const key = JSON.stringify(items) + S.nav;
  const nav = $("#nav");
  if (nav._key !== key) {
    nav._key = key;
    nav.replaceChildren();
    for (const it of items) {
      if (it.head) { nav.append(el("div", { class: "nav-head", text: it.head })); continue; }
      const [k, label, ic, running] = it;
      const row = el("button", { class: "nav-item" + (k === S.nav ? " active" : ""), html: icon(ic), title: label, "aria-label": label, "aria-pressed": k === S.nav ? "true" : "false" });
      row.dataset.k = k;
      row.classList.toggle("extra-filter", k.startsWith("q:") || k.startsWith("tag:") || k === "unfinished");
      row.append(el("span", { class: "ell", text: label }));
      if (running) row.append(el("span", { class: "run-dot", title: "Running" }));
      row.append(el("span", { class: "count" }));
      row.addEventListener("click", () => { S.nav = k; renderNav(); arrange(); });
      if (k.startsWith("q:")) row.addEventListener("contextmenu", (e) => { e.preventDefault(); queueMenu(k.slice(2), e.clientX, e.clientY); });
      nav.append(row);
    }
  }
  for (const row of $$(".nav-item", nav)) {
    const c = count(row.dataset.k);
    $(".count", row).textContent = c || "";
  }
  renderQueuePanel();
}

function queueMenu(name, x, y) {
  const q = S.summary.queues.find((q) => q.name === name);
  showMenu([
    { label: "Start queue", disabled: q?.running, click: () => invoke("queue_run", { name, on: true }) },
    { label: "Stop queue", disabled: !q?.running, click: () => invoke("queue_run", { name, on: false }) },
    "-",
    { label: "Edit schedule…", click: () => openScheduler(name) },
  ], x, y);
}

function renderQueuePanel() {
  const panel = $("#queuePanel");
  if (!S.nav.startsWith("q:")) { panel.hidden = true; return; }
  const q = S.summary?.queues.find((q) => q.name === S.nav.slice(2));
  if (!q) { panel.hidden = true; return; }
  panel.hidden = false;
  $("#queueInfo").innerHTML = `<b>${esc(q.name)}</b>${q.running ? "Running" : "Stopped"} · ${q.active} active · ${q.waiting} waiting · max ${q.max} at once`;
  $("#queueStart").disabled = q.running;
  $("#queueStop").disabled = !q.running;
}

// ---------------------------------------------------------------- status bar & global limit

function renderStatus() {
  const s = S.summary;
  if (!s) return;
  let t = s.running === 0 ? `Idle · ${s.count} download${s.count === 1 ? "" : "s"}` : `${s.running} running · ${s.speedText || "0 B/s"}` + (s.total > 0 ? ` · ${fmtSize(s.done)} of ${fmtSize(s.total)}` : "");
  if (s.seeding) t = (s.running ? t + " · " : "") + `Seeding ${s.seeding} · ↑ ${s.upSpeedText}`;
  if (s.schedule) t += ` · speed schedule active (${s.effectiveLimitText})`;
  $("#statusLeft").textContent = t;
  $("#statusLeft").title = t;
  $("#statusTorrent").textContent = s.torrent || "";
  $("#statusDown").textContent = "↓ " + (s.speedText || "0 B/s");
  $("#statusUp").textContent = "↑ " + (s.upSpeedText || "0 B/s");
  const rows = [...(S.rows?.values() || [])];
  $("#statusCounts").textContent = rows.filter((r) => ["Paused", "Stopped"].includes(r.status)).length + " paused — " + rows.filter((r) => r.status === "Queued").length + " queued";
  $("#statusQueues").textContent = (s.queues || []).map((q) => q.name + (q.running ? " running" : " stopped")).join(" — ");
  if (document.activeElement !== $("#limitInput") && S.settings && s.limitKBps !== S.settings.GlobalSpeedLimitKBps) {
    S.settings.GlobalSpeedLimitKBps = s.limitKBps;
    showLimit();
  }
}

let uploadCombo = null;
let globalCombo = null, detailLimit = { show() {} }, detailUpload = { show() {} }, detailSeeding = { show() {} };
function showLimit() {
  if (globalCombo) globalCombo.show();
  if (uploadCombo) uploadCombo.show();
}

async function setGlobalLimit(k) {
  S.settings.GlobalSpeedLimitKBps = k;
  await invoke("set_global_limit", { kbps: k });
  showLimit();
}

// ---------------------------------------------------------------- selection

function selectOnly(id) { S.sel = new Set(id ? [id] : []); S.cur = id; S.anchor = id; paintSel(); renderDetails(); }
function paintSel() {
  for (const [id] of trs) { const r = S.rows.get(id); if (r) renderRow(r); }
  updateResumeAction();
  updateWindowArt();
}
function selected() { return [...S.sel].filter((id) => S.rows.has(id)).map((id) => S.rows.get(id)); }
function selIds() { return selected().map((r) => r.id); }

function updateResumeAction() {
  const rows = selected();
  $("#btnResume").hidden = rows.length > 0 && rows.every(r => r.status === "Completed" && !r.torrent);
  $("#btnResume").title = rows.length > 0 && rows.every(r => r.status === "Completed" && r.torrent) ? "Resume sharing" : "Resume selected";
}

function onRowClick(e, id) {
  if (e.shiftKey && S.anchor) {
    const ids = visibleIds();
    const a = ids.indexOf(S.anchor), b = ids.indexOf(id);
    if (a >= 0 && b >= 0) {
      if (!e.ctrlKey) S.sel.clear();
      ids.slice(Math.min(a, b), Math.max(a, b) + 1).forEach((x) => S.sel.add(x));
    }
    S.cur = id;
  } else if (e.ctrlKey) {
    if (S.sel.has(id)) S.sel.delete(id); else S.sel.add(id);
    S.cur = id;
    S.anchor = id;
  } else {
    S.sel = new Set([id]);
    S.cur = id;
    S.anchor = id;
  }
  paintSel();
  renderDetails();
}

function moveCursor(delta, extend) {
  const ids = visibleIds();
  if (!ids.length) return;
  let i = S.cur ? ids.indexOf(S.cur) : -1;
  i = Math.max(0, Math.min(ids.length - 1, i < 0 ? 0 : i + delta));
  const id = ids[i];
  if (extend && S.anchor) {
    const a = ids.indexOf(S.anchor);
    S.sel = new Set(ids.slice(Math.min(a, i), Math.max(a, i) + 1));
    S.cur = id;
    paintSel();
    renderDetails();
  } else selectOnly(id);
  trs.get(id)?.scrollIntoView({ block: "nearest" });
}

// ---------------------------------------------------------------- actions

const act = {
  // A finished torrent resumes sharing (Pause stopped it); other finished downloads have nothing to resume.
  resume: () => invoke("start", { ids: selected().filter((r) => r.status !== "Completed" || r.torrent).map((r) => r.id) }),
  pause: () => invoke("pause", { ids: selIds() }),
  toggle: () => invoke("toggle", { ids: selIds() }),
  pauseAll: () => invoke("pause_all"),
  open: () => { const r = S.rows.get(S.cur); if (r && r.status === "Completed") invoke("open_item", { id: r.id }); },
  folder: () => { if (S.cur) invoke("show_item", { id: S.cur }); },
  copyUrl: () => { const t = selected().map((r) => r.url).join("\r\n"); if (t) invoke("copy_text", { text: t }); },
  async delete() {
    const sel = selected();
    if (!sel.length) return;
    const what = sel.length === 1 ? `“${sel[0].name}”` : `${sel.length} downloads`;
    if (sel.some((r) => r.status === "Completed")) {
      const trash = el("input", { type: "checkbox" });
      const v = await new Promise((res) => modal({
        title: "Delete",
        body: el("div", {}, el("p", { text: `Remove ${what} from the list?` }),
          el("label", { class: "check" }, trash, "Also delete the downloaded file(s)"),
          el("p", { class: "hint", text: "Deleted files go to the Recycle Bin. Unfinished downloads lose their partial data." })),
        buttons: [
          { label: "Cancel", value: false },
          { label: "Remove", value: true, danger: true },
        ],
        onClose: res,
      }));
      if (!v) return;
      await invoke("remove", { ids: sel.map((r) => r.id), deleteFiles: trash.checked });
    } else {
      if (!(await confirmBox("Delete", `Remove ${what}? Partially downloaded data will be deleted.`, "Remove", true))) return;
      await invoke("remove", { ids: sel.map((r) => r.id), deleteFiles: false });
    }
  },
  async clearFinished() {
    const n = [...S.rows.values()].filter((r) => r.status === "Completed").length;
    if (!n) return;
    if (await confirmBox("Clear finished", `Remove ${n} finished download${n === 1 ? "" : "s"} from the list? The files stay on disk.`, "Remove")) invoke("clear_finished");
  },
  async restart() {
    const ids = selIds();
    if (!ids.length) return;
    if (await confirmBox("Restart", `Throw away progress and download ${ids.length} item(s) again from zero?`, "Restart", true)) invoke("restart", { ids });
  },
  async refreshLink() {
    const r = S.rows.get(S.cur);
    if (!r) return;
    if (r.torrent) return alertBox("Refresh link", "Torrents find their peers by themselves — there's no link to refresh.");
    const url = await promptBox("Refresh link", "Paste a fresh link for this file (e.g. copied again from the website). If the size matches, the bytes you already have are kept.", r.url, { mono: true, action: "Replace link" });
    if (!url || url.trim() === r.url) return;
    if (!/^https?:\/\//i.test(url.trim())) return alertBox("Refresh link", "That isn't an http(s) link.");
    let p;
    try { p = await invoke("probe", { url: url.trim(), headers: "" }); }
    catch (e) { return alertBox("Refresh link", "Couldn't reach the new link: " + e); }
    let restart = false;
    if (r.total > 0 && p.total !== r.total) {
      if (!(await confirmBox("Refresh link", `The new link is ${fmtSize(p.total)} but the original was ${fmtSize(r.total)} — it's a different file.\nUse it anyway and start from zero?`, "Replace link", true))) return;
      restart = true;
    }
    await invoke("refresh_link", { id: r.id, url: url.trim(), restart });
  },
  async addMirror() {
    const r = S.rows.get(S.cur);
    if (!r) return;
    if (r.video || r.torrent) return alertBox("Backup link", "Backup links are for normal file downloads, not video pages or torrents.");
    const text = await promptBox("Add backup link", "Another link for exactly the same file (a mirror). FluxDM spreads connections across all links, and keeps going from the others if one dies. Several links can be pasted, one per line.", "", { multiline: true, mono: true, action: "Add backup links" });
    if (!text || !text.trim()) return;
    try { await invoke("add_mirrors", { id: r.id, text }); } catch (e) { alertBox("Backup link", String(e)); }
  },
  async editTags() {
    const sel = selected();
    if (!sel.length) return;
    let common = sel[0].tags;
    for (const r of sel.slice(1)) common = common.filter((t) => r.tags.some((x) => x.toLowerCase() === t.toLowerCase()));
    const text = await promptBox("Edit tags", "Comma-separated labels for organising downloads (e.g. Games, Movies) — separate from the automatic file-type category. Leave empty to clear.", common.join(", "), { action: "Save tags" });
    if (text == null) return;
    const seen = new Set();
    const tags = text.split(",").map((t) => t.trim()).filter((t) => t && !seen.has(t.toLowerCase()) && seen.add(t.toLowerCase()));
    invoke("set_items", { ids: sel.map((r) => r.id), patch: { tags } });
  },
  async sha256() {
    const r = S.rows.get(S.cur);
    if (!r || r.status !== "Completed") return;
    $("#statusLeft").textContent = `Computing SHA-256 of ${r.name}…`;
    try {
      const h = await invoke("sha256", { id: r.id });
      await invoke("copy_text", { text: h });
      const verdict = r.hash.length === 64 ? (h.toLowerCase() === r.hash.toLowerCase() ? "\n\nMatches the expected checksum." : "\n\nDoes NOT match the expected checksum!") : "";
      alertBox(`SHA-256 — ${r.name}`, `${h}\n\n(copied to clipboard)${verdict}`);
    } catch (e) { alertBox("SHA-256", String(e)); }
  },
  moveTo: (q) => invoke("set_items", { ids: selIds(), patch: { queue: q } }),
  async recheck() { try { await invoke("torrent_recheck", { ids: selIds() }); } catch (e) { alertBox("Force recheck", String(e)); } },
  async reannounce() {
    try {
      const n = await invoke("torrent_reannounce", { ids: selIds() });
      if (!n) alertBox("Force reannounce", "Only running torrents can reannounce — resume it first.");
    } catch (e) { alertBox("Force reannounce", String(e)); }
  },
  async setLocation() {
    const sel = selected().filter((r) => r.torrent);
    if (!sel.length) return;
    const folder = await T.dialog.open({ directory: true, title: "Move the downloaded data to…", defaultPath: sel[0].folder });
    if (!folder) return;
    const drive = (p) => String(p).slice(0, 2).toLowerCase();
    const cross = sel.filter((r) => r.status !== "Completed" && drive(r.folder) !== drive(folder));
    let restart = false;
    if (cross.length) {
      restart = await confirmBox("Start over in the new folder?",
        `${cross.length === 1 ? "This torrent isn't" : "These torrents aren't"} finished, and the new folder is on another drive. Moving would copy the torrent's full-size placeholder files (every byte, then re-check them all) — very slow and heavy on the disk.\n\nInstead, FluxDM can delete the partial files and start downloading in the new folder, re-downloading what was already done.`,
        "Start over there");
      if (!restart) return;
    }
    try { await invoke("torrent_move", { ids: sel.map((r) => r.id), folder, restartIfUnfinished: restart }); } catch (e) { alertBox("Set location", String(e)); }
  },
  async addTrackers() {
    const ids = selected().filter((r) => r.torrent).map((r) => r.id);
    if (!ids.length) return;
    const text = await promptBox("Add trackers", "Tracker URLs to add (one per line or separated by spaces), e.g. udp://tracker.opentrackr.org:1337/announce. More trackers can find more peers; they're kept for this torrent from now on.", "", { multiline: true, mono: true, action: "Add trackers" });
    if (!text || !text.trim()) return;
    try { await invoke("torrent_add_trackers", { ids, text }); refreshDetails(); } catch (e) { alertBox("Add trackers", String(e)); }
  },
  copyTorrent: (what) => invoke("torrent_copy", { ids: selIds(), what }),
  async exportTorrent() {
    const sel = selected().filter((r) => r.torrent);
    if (!sel.length) return;
    let dest, isFolder = sel.length > 1;
    if (isFolder) dest = await T.dialog.open({ directory: true, title: "Export the .torrent files to…" });
    else dest = await T.dialog.save({ defaultPath: sel[0].name.replace(/[\\/:*?"<>|]/g, "_") + ".torrent", filters: [{ name: "Torrent files", extensions: ["torrent"] }] });
    if (!dest) return;
    try { await invoke("torrent_export", { ids: sel.map((r) => r.id), dest, isFolder }); } catch (e) { alertBox("Export .torrent", String(e)); }
  },
  async iconFind() {
    const r = S.rows.get(S.cur);
    if (!r) return;
    const guess = await invoke("clean_title", { name: r.name });
    const name = await promptBox("Find cover", "Game name to search for (Steam's store is searched):", guess, { action: "Find cover" });
    if (!name || !name.trim()) return;
    const found = await invoke("icon_find", { id: r.id, name: name.trim() });
    if (!found) alertBox("Find cover", `No game closely matching “${name.trim()}” was found. Try a shorter or more exact name, or choose an image.`);
  },
  async iconChoose() {
    const ids = selIds();
    if (!ids.length) return;
    const path = await T.dialog.open({ title: "Choose a picture", filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "bmp", "gif", "ico", "webp"] }, { name: "All files", extensions: ["*"] }] });
    if (!path) return;
    try { await invoke("icon_choose", { ids, path }); } catch (e) { alertBox("Choose image", String(e)); }
  },
  iconMode: (mode) => invoke("icon_mode", { ids: selIds(), mode }),
  async bannerChoose() {
    const ids = selIds();
    if (!ids.length) return;
    const path = await T.dialog.open({ title: "Choose a banner picture (wide pictures work best)", filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp"] }] });
    if (!path) return;
    try { await invoke("banner_choose", { ids, path }); } catch (e) { alertBox("Choose banner", String(e)); }
  },
};

async function limitMenuItems(title, what, key, presets, patchKey) {
  const sel = selected();
  const vals = new Set(sel.map((r) => r[key]));
  const common = vals.size === 1 ? [...vals][0] : null;
  const set = (k) => invoke("set_items", { ids: sel.map((r) => r.id), patch: { [patchKey]: k } });
  const items = presets.map(([label, k]) => ({ label, checked: common === k, click: () => set(k) }));
  const custom = common != null && !presets.some((p) => p[1] === common);
  items.push("-", {
    label: custom ? `Custom: ${fmtLimit(common)}…` : "Custom…", checked: custom,
    click: async () => {
      const t = await promptBox(title, `${what} for the selected download(s) — e.g. 750k, 2 MB/s or 0 for unlimited:`, common != null ? fmtLimit(common) : "1 MB/s", {
        action: "Set speed limit",
        validate: (v) => (parseSpeed(v, patchKey !== "limitKBps") == null ? "Couldn't read that speed. Try 750k or 2 MB/s." : ""),
      });
      if (t != null) set(parseSpeed(t, patchKey !== "limitKBps"));
    },
  });
  return items;
}

async function rowMenu(x, y) {
  const sel = selected();
  if (!sel.length) return;
  const cur = S.rows.get(S.cur) || sel[0];
  const torrents = sel.every((r) => r.torrent);
  const queues = (S.summary?.queues || []).map((q) => ({ label: q.name, checked: sel.every((r) => r.queue === q.name), click: () => act.moveTo(q.name) }));
  const items = [
    { label: "Open", key: "Enter", disabled: cur.status !== "Completed", click: act.open },
    { label: "Show in folder", click: act.folder },
    "-",
    sel.some(r => r.status !== "Completed" || r.torrent) && { label: torrents && sel.every((r) => r.status === "Completed") ? "Resume sharing" : "Resume", click: act.resume },
    { label: torrents && sel.every((r) => r.status === "Completed") ? "Stop sharing" : "Pause", click: act.pause },
    { label: "Restart from zero", click: act.restart },
    sel.length === 1 && !cur.torrent && !cur.video && cur.status === "Paused" && { label: "Choose file conflict action...", click: () => showFileConflict({ id: cur.id, name: cur.name }) },
    !torrents && { label: "Refresh link…", click: act.refreshLink },
    { label: "Bandwidth limit", sub: [
      { label: "Download", sub: await limitMenuItems("Download limit", "Download speed limit", "limitKBps", LIMIT_PRESETS, "limitKBps") },
      torrents && { label: "Upload", title: "While downloading", sub: await limitMenuItems("Upload limit", "Upload limit while downloading", "uploadLimitKBps", UPLOAD_PRESETS, "uploadLimitKBps") },
      torrents && { label: "Seeding", title: "After it finishes", sub: await limitMenuItems("Seeding limit", "Upload limit after it finishes", "seedingLimitKBps", UPLOAD_PRESETS, "seedingLimitKBps") },
    ] },
    sel.some((r) => r.status !== "Completed") && { label: "Open when it finishes", title: "Opens the file (or the torrent's folder) as soon as it's done", checked: sel.every((r) => r.openWhenDone), click: () => { const on = !sel.every((r) => r.openWhenDone); invoke("set_items", { ids: sel.map((r) => r.id), patch: { openWhenDone: on } }); } },
    torrents && { label: "Download in order", title: "Fetch pieces from the start of each file to the end, so a video can be watched while it downloads. Usually a bit slower overall.", checked: sel.every((r) => r.sequential), click: () => { const on = !sel.every((r) => r.sequential); invoke("set_items", { ids: sel.map((r) => r.id), patch: { sequential: on } }); } },
    { label: "Move to queue", sub: [{ label: "(no queue)", checked: sel.every((r) => !r.queue), click: () => act.moveTo("") }, ...queues] },
    !torrents && { label: "Add backup link…", click: act.addMirror },
    { label: "Edit tags…", click: act.editTags },
    { label: "Icon", sub: [
      { label: "Find cover by name…", click: act.iconFind },
      { label: "Choose image…", click: act.iconChoose },
      { label: "Use the file's own icon", click: () => act.iconMode("file") },
      { label: "No icon", click: () => act.iconMode("off") },
      { label: "Look up automatically again", click: () => act.iconMode("reset") },
    ] },
    { label: "Banner", sub: [
      { label: "Choose banner image…", title: "Use a picture from your PC as the big banner (blank borders are cropped off)", click: act.bannerChoose },
      { label: "Find a banner automatically again", click: () => invoke("banner_reset", { ids: selIds() }) },
    ] },
  ];
  if (torrents) {
    items.push("-",
      { label: "Force recheck", title: "Re-reads everything already on disk and keeps only the pieces that verify", click: act.recheck },
      { label: "Force reannounce", title: "Ask the trackers, DHT and local network for more peers right now", click: act.reannounce },
      { label: "Set location…", click: act.setLocation },
      { label: "Add trackers…", click: act.addTrackers },
      { label: "Copy", sub: [
        { label: "Name", click: () => act.copyTorrent("name") },
        { label: "Info hash", click: () => act.copyTorrent("hash") },
        { label: "Magnet link", click: () => act.copyTorrent("magnet") },
      ] },
      { label: "Export .torrent…", click: act.exportTorrent },
      { label: "Show peers", click: () => setTab("peers") });
  }
  items.push("-", { label: "Copy URL", click: act.copyUrl });
  if (!torrents) items.push({ label: "Compute SHA-256", disabled: cur.status !== "Completed", click: act.sha256 });
  items.push("-", { label: "Delete…", key: "Del", click: act.delete });
  showMenu(items, x, y);
}

// ---------------------------------------------------------------- details pane

function setTab(t) {
  const r = S.rows.get(S.cur);
  const allowed = r?.torrent ? ["peers", "trackers", "content", "pieces", "options", "log"] : ["general", "options", "mirrors", "log"];
  if (!allowed.includes(t)) t = allowed[0];
  S.tab = t;
  $$(".tab").forEach((b) => { b.classList.toggle("active", b.dataset.tab === t); b.setAttribute("aria-selected", String(b.dataset.tab === t)); });
  $$(".tabpage").forEach((p) => (p.hidden = p.id !== "tab-" + t));
  $("#detailLower").classList.toggle("full-tab", t === "content" || t === "trackers");
  drawSeg(); drawGraph();
  refreshDetails();
}

function renderDetails() {
  updateWindowArt();
  const r = S.cur ? S.rows.get(S.cur) : null;
  $("#detailsPane").hidden = !r;
  $("#detailsEmpty").hidden = !!r;
  if (!r) { S.details = null; return; }
  if (S.detailsId !== r.id) { S.detailsId = r.id; S.details = null; drawSeg(); drawGraph(); refreshDetails(); }
  $$("[data-torrent]").forEach((e) => (e.hidden = !r.torrent));
  $$("[data-http]").forEach((e) => (e.hidden = r.torrent));
  $$("[data-torrent-only]").forEach((e) => (e.hidden = !r.torrent));
  if (r.torrent ? ["general", "mirrors"].includes(S.tab) : ["peers", "trackers", "content", "pieces"].includes(S.tab)) setTab(r.torrent ? "peers" : "general");
  $("#dName").textContent = r.name;
  $("#dName").title = r.name;
  if (document.activeElement !== $("#dUrl")) $("#dUrl").value = r.url;
  $("#dPath").textContent = r.path;
  $("#dPath").title = r.path;
  $("#dUrl").title = r.url;
  $("#dProgress").textContent = r.downloadedText + (r.progressText && r.total > 0 ? ` (${r.progressText})` : "");
  $("#dResumable").textContent = r.torrent ? "Yes (torrent)" : r.video ? "Yes (yt-dlp keeps its own partial files)" : r.resumable ? "Yes" : r.total > 0 ? "No — the server doesn't support resuming" : "Not known yet";
  $("#dStatus").textContent = r.statusText;
  $("#dStatus").className = "selectable pill " + (r.seeding ? "st-Seeding" : statusClass(r));
  renderDetailHeader(r);
  if (document.activeElement !== $("#dConn")) { $("#dConn").value = r.connections; $("#dConnVal").textContent = r.connections; }
  detailLimit.show();
  detailUpload.show();
  detailSeeding.show();
  $$("[data-unfinished]").forEach((e) => (e.hidden = r.status === "Completed"));
  $("#dOpenDone").checked = !!r.openWhenDone;
  $("#dMirrors").textContent = r.mirrors.length ? `${r.mirrors.length} backup link${r.mirrors.length === 1 ? "" : "s"}` : "None";
  if (document.activeElement !== $("#dHash")) $("#dHash").value = r.hash;
  $("#dOpen").disabled = r.status !== "Completed";
  $("#segHint").textContent = r.torrent ? "Each block groups consecutive pieces; fill shows verified data. Files shows each file."
    : r.video ? "yt-dlp downloads the video and audio streams, then joins them with ffmpeg."
    : "Each block is a consecutive part of the file; fill shows downloaded bytes.";
  if (r.torrent) {
    const parts = [];
    if (r.seeds !== "" || r.peers !== "") parts.push(`Seeds ${r.seeds} · Peers ${r.peers}`);
    parts.push(`Uploaded ${fmtSize(r.uploaded)}` + (r.ratio ? ` · Ratio ${r.ratio}` : ""));
    if (r.availability) parts.push(`Availability ${r.availability}`);
    if (r.timeActive) parts.push(`Active ${r.timeActive}`);
    if (r.uploadLimitKBps) parts.push(`Upload limit ${fmtLimit(r.uploadLimitKBps)}`);
    if (r.seedingLimitKBps) parts.push(`Seeding limit ${fmtLimit(r.seedingLimitKBps)}`);
    if (r.infoHash) parts.push(`Info hash ${r.infoHash.toLowerCase()}`);
    $("#dTorrent").textContent = parts.join("   ·   ");
  }
}

function detailType(r) {
  return (r.game ?? !!r.banner) ? "game" : r.torrent ? "torrent" : /AI/i.test(r.category) ? "ai" : /Video/i.test(r.category) ? "video" : /Program/i.test(r.category) ? "program" : /Compress|Archive/i.test(r.category) ? "archive" : "file";
}
function rowActivity(r) {
  if (r.seeding) return "Sharing ↑ " + (r.upSpeedText || fmtSpeed(r.upSpeed || 0)) + " · ratio " + (r.ratio || "0");
  if (r.status === "Paused" || r.status === "Stopped") return "Paused at " + Math.round(r.progress || 0) + "%";
  if (r.status === "Queued") return r.queue || "Main queue";
  if (r.status === "Completed") return r.completed ? "Done · " + r.completed : "Done";
  if (r.status === "Error") return (r.error || r.statusText || "Download failed").split("\n")[0];
  return "";
}
function renderDetailHeader(r) {
  const d = S.details || {}, banner = $("#detailBanner"), type = detailType(r);
  $("#dName").textContent = r.title || d.title || r.name;
  const pct = r.status === "Completed" ? 100 : Math.max(0, Math.min(100, Number(r.progress) || 0));
  banner.className = "type-" + type + (r.status === "Completed" ? " finished" : "");
  banner.style.setProperty("--progress", pct + "%");
  const art = $("#bannerArt"), url = r.banner || d.banner || r.icon || d.cover || r.cover || fallbackArt(r.torrent ? "magnet" : CATEGORY_ICON[r.category] || "file");
  const artColor = r.artColor || d.artColor || "";
  const showArt = () => {
    art.replaceChildren(url ? el("img", { src: url, alt: "", class: r.banner || d.banner ? "" : "icon-wash" }) : el("span", { html: icon(r.torrent ? "magnet" : CATEGORY_ICON[r.category] || "file") }));
    art.dataset.artColor = artColor;
    sampleCardArt(art, url);
  };
  if (art.dataset.url !== (url || type)) {
    art.dataset.url = url || type;
    // The current picture stays until the new one is decoded and measured, then they swap in one frame
    // (otherwise switching downloads showed an empty banner or the wrong layout for a moment).
    const swap = ++bannerSwap;
    preloadArt(url).then(() => { if (swap === bannerSwap) showArt(); });
  } else if (art.dataset.artColor !== artColor) {
    art.dataset.artColor = artColor;
    sampleCardArt(art, url);
  }
  $("#detailGlyph").innerHTML = icon(r.torrent ? "magnet" : type === "game" ? "game" : CATEGORY_ICON[r.category] || "file");
  const picture = r.icon || d.cover || r.cover;
  if (picture && $("#dCover").getAttribute("src") !== picture) $("#dCover").src = picture;
  $("#dCover").hidden = !picture || $("#dCover").dataset.failed === picture;
  $("#detailGlyph").hidden = !$("#dCover").hidden;
  $("#dStatus").textContent = statusLabel(r);
  let host = ""; try { host = new URL(r.url).hostname; } catch { /* pending metadata */ }
  $("#dHost").textContent = r.torrent ? "Torrent" : host;
  $("#dResumeChip").textContent = r.torrent ? ((d.torrent?.trackers || []).filter(t => t.status === "Working").length + " trackers working") : r.resumable || r.video ? "Resumable" : "";
  $("#dCategory").textContent = r.category;
  $("#dQueue").textContent = r.queue || "Main queue";
  $("#dFile").textContent = r.path?.split(/[\\/]/).at(-1) || r.name;
  $("#dPercent").textContent = Math.round(pct) + "%";
  $("#dEta").textContent = r.eta ? r.eta + " left" : rowActivity(r) || r.statusText;
  $("#dEta").title = $("#dEta").textContent; // long errors are cut to two lines; the whole text shows on hover
  $("#dToggle").innerHTML = icon(canPause(r) ? "pause" : "play") + (r.seeding ? "Stop sharing" : canPause(r) ? "Pause" : r.torrent && r.status === "Completed" ? "Resume sharing" : "Resume");
  $("#dToggle").hidden = !r.torrent && r.status === "Completed";
  $("#dToggle").disabled = false;
  $("#dCopy").textContent = r.torrent ? "Copy magnet" : "Copy link";
  $("#dProgressTop").textContent = r.downloadedText;
  const pieces = d.torrent?.pieces;
  $("#dPiecesCount").textContent = r.torrent ? pieces ? pieces.have.toLocaleString() + " of " + pieces.total.toLocaleString() + " pieces" : "Piece information pending" : d.segInfo || "";
  const stat = (label, value, sub = "", color = "") => '<div class="detail-stat"><span>' + esc(label) + '</span><strong class="' + color + '">' + esc(value) + '</strong><span>' + esc(sub) + '</span></div>';
  const hist = d.history || [], avg = hist.length ? hist.reduce((a,b) => a+b,0) / hist.length : 0;
  const stats = r.torrent ? [
    stat("Download", r.speedText || "\u2014", "limit: " + fmtLimit(r.limitKBps || 0), "down"),
    stat("Upload", r.upSpeedText || "\u2014", "limit: " + fmtLimit((r.seeding ? r.seedingLimitKBps : r.uploadLimitKBps) || 0), "up"),
    stat("Uploaded", fmtSize(r.uploaded || 0), "ratio " + (r.ratio || "0")),
    stat("Peers", d.torrent?.live ? String(d.torrent.live.live) : "\u2014", (r.seeds || "0") + " seeds \u00b7 " + (r.peers || "0") + " peers"),
    stat("Time left", r.eta || "\u2014", r.timeActive ? "active " + r.timeActive : ""), stat("Availability", r.availability || "\u2014", "distributed copies")
  ] : [stat("Downloaded", fmtSize(r.downloaded), "of " + r.sizeText), stat("Speed", r.speedText || "\u2014", "avg " + fmtSpeed(avg), "down"), stat("Time left", r.eta || "\u2014"), stat("Connections", r.conn || "\u2014", r.running ? "receiving" : "inactive"), stat("Started", d.started || "\u2014", r.timeActive ? "active " + r.timeActive : ""), stat("Retries", String(d.retries ?? 0), r.error || "no errors")];
  $("#detailStats").innerHTML = stats.join("");
  $("#dLinkLabel").textContent = r.torrent ? "Info hash" : "Link";
  $("#dLink").textContent = r.torrent ? r.infoHash || "Pending metadata" : r.url;
  $("#dRuleLabel").textContent = r.torrent ? "Sharing rule" : "Checksum";
  $("#dRule").textContent = r.torrent ? r.seedingLimitKBps === -1 ? "No upload when finished" : d.sharingRatio <= 0 ? "Stop sharing immediately" : "Share until ratio " + d.sharingRatio + (d.sharingMinutes > 0 ? " or " + d.sharingMinutes + " minutes" : "") + (d.sharingAction === "Remove" ? ", then remove torrent (keep files)" : ", then stop") : r.hash || "No expected checksum set";
  $("#mirrorList").textContent = (r.mirrors || []).join("\n") || "No backup links.";
  $("#detailLog").textContent = [r.added && "Added " + r.added, d.started && "Last started " + d.started, r.statusText, d.note, r.error, r.completed && "Completed " + r.completed, "Retries: " + (d.retries ?? 0)].filter(Boolean).join("\n");
}
function renderConnectionTable(r) {
  const segs = S.details?.segments || [], rates = S.details?.connectionRates || [];
  miniTable($("#connectionTable"), "Connections", [["#", 26], ["Part", 130], ["Done", 72], ["Speed", 78]], segs.map(([start,end,pos,active], i) => [i+1,
    {html: '<div class="bar"><i style="width:' + Math.max(0,Math.min(100,100*(pos-start)/(end-start))) + '%"></i></div>'}, fmtSize(Math.max(0,pos-start)*Math.max(0,r.total)), active && rates[i] != null ? fmtSpeed(rates[i]) : active ? "Receiving" : "\u2014"]));
}

let detailsBusy = false;
let detailsPending = false;
let detailsKey = "";
function detailsActive(r) { return r.running || r.seeding || ["Connecting", "Downloading", "Verifying"].includes(r.status); }
function detailKey(r) {
  return JSON.stringify([r.id, r.status, r.running, r.seeding, r.downloaded, r.total, r.path, r.limitKBps, r.uploadLimitKBps, r.seedingLimitKBps, r.connections, r.sequential, r.infoHash, r.cover]);
}
async function refreshDetails(periodic = false) {
  const r = S.cur ? S.rows.get(S.cur) : null;
  if (!r || document.hidden) return;
  if (detailsBusy) {
    if (periodic !== true) detailsPending = true;
    return;
  }
  const key = detailKey(r);
  if (periodic === true && S.details && !detailsActive(r) && detailsKey === key) return;
  detailsBusy = true;
  try {
    const d = await invoke("details", { id: r.id });
    if (S.cur !== r.id) return;
    const previous = S.details;
    const sampledAt = performance.now();
    d.sampledAt = sampledAt;
    const elapsed = previous?.sampledAt ? (sampledAt - previous.sampledAt)/1000 : 0;
    d.connectionRates = (d.segments || []).map(([start,,pos,active]) => {
      const old = previous?.segments?.find(s => s[0] === start);
      return active && old && elapsed > 0 ? Math.max(0,(pos-old[2])*Math.max(0,r.total)/elapsed) : null;
    });
    S.details = d;
    detailsKey = key;
    const cover = d && d.cover;
    const img = $("#dCover");
    const picture = r.icon || cover;
    if (picture) { if (img.getAttribute("src") !== picture) img.src = picture; img.hidden = false; } else img.hidden = true;
    $("#segInfo").textContent = d?.segInfo || "";
    renderDetailHeader(r);
    renderConnectionTable(r);
    drawSeg(); drawGraph();
    if (d?.torrent) renderTorrentTab(d.torrent);
  } catch { /* the item went away */ }
  finally {
    detailsBusy = false;
    if (detailsPending) { detailsPending = false; refreshDetails(); }
  }
}

function css(name) { return getComputedStyle(document.documentElement).getPropertyValue(name).trim(); }

function fitCanvas(c) {
  const dpr = devicePixelRatio || 1;
  const w = c.clientWidth, h = c.clientHeight;
  if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) { c.width = Math.round(w * dpr); c.height = Math.round(h * dpr); }
  const g = c.getContext("2d");
  g.setTransform(dpr, 0, 0, dpr, 0, 0);
  return [g, w, h];
}

function drawSeg() {
  const r = S.rows.get(S.cur); if (!r) return;
  const segs = S.details?.segments || [];
  const top = $("#connectionBar");
  if (top.clientWidth) {
    const [g,w,h] = fitCanvas(top); g.clearRect(0,0,w,h);
    const count = r.torrent ? 1 : Math.max(1, segs.length || r.connections || 1), gap = r.torrent ? 0 : 3;
    const width = (w-gap*(count-1))/count;
    for (let i=0;i<count;i++) {
      const seg = r.torrent ? null : segs[i], fraction = r.status === "Completed" ? 1 : seg ? Math.max(0,Math.min(1,(seg[2]-seg[0])/(seg[1]-seg[0]))) : (r.progress || 0)/100;
      g.fillStyle = css("--bar-bg"); g.fillRect(i*(width+gap),0,width,h);
      g.fillStyle = css("--accent"); g.fillRect(i*(width+gap),0,width*fraction,h);
    }
  }
  const pieces = r.torrent ? S.details?.torrent?.pieces : null;
  const blocks = pieces?.blocks || Array.from({length:80},(_,i) => {
    const start = i/80, end = (i+1)/80;
    if (r.status === "Completed") return 1;
    if (!segs.length) return r.torrent ? -1 : Math.max(0,Math.min(1,((r.progress||0)/100-start)*80));
    return Math.max(0,Math.min(1,segs.reduce((n,[s,e,p])=>n+Math.max(0,Math.min(end,p,e)-Math.max(start,s)),0)*80));
  });
  for (const id of ["#segBar", "#pieceMap"]) {
    const c = $(id); if (!c.clientWidth) continue;
    const [g,w,h] = fitCanvas(c); g.clearRect(0,0,w,h);
    const display = id === "#segBar" && blocks.length > 80 ? Array.from({ length: 80 }, (_,i) => {
      const group = blocks.slice(Math.floor(i*blocks.length/80), Math.floor((i+1)*blocks.length/80));
      return group.reduce((a,b)=>a+b,0)/group.length;
    }) : blocks;
    const cols = Math.max(1,Math.min(40,display.length,Math.floor(w/7))), rows = Math.ceil(display.length/cols);
    display.forEach((v,i) => {
      const x = (i%cols)*w/cols, y = Math.floor(i/cols)*h/rows, bw = w/cols-2, bh = h/rows-2;
      g.fillStyle = css("--bar-bg"); g.fillRect(x,y,bw,bh);
      if (v>0) { g.fillStyle = css("--accent"); g.fillRect(x,y,bw*v,bh); }
    });
  }
  $("#piecesSummary").textContent = pieces ? pieces.have + " of " + pieces.total + " pieces verified. Each block groups consecutive pieces; fill shows its verified fraction." : "Verified piece information is unavailable until the torrent metadata and state are loaded.";
}

function drawGraph() {
  const c = $("#graph");
  if (!c.clientWidth) return;
  const [g, w, h] = fitCanvas(c);
  g.clearRect(0, 0, w, h);
  const hist = S.details?.history || [];
  const max = Math.max(1024, ...hist, ...(S.details?.uploadHistory || [])) * 1.15;
  g.strokeStyle = css("--line");
  g.lineWidth = 1;
  for (let i = 1; i < 4; i++) { const y = Math.round((h * i) / 4) + 0.5; g.beginPath(); g.moveTo(0, y); g.lineTo(w, y); g.stroke(); }
  g.fillStyle = css("--text-3");
  g.font = "11px Inter";
  g.fillText(fmtSpeed(max / 1.15), 4, 12);
  const upload = S.rows.get(S.cur)?.torrent ? S.details?.uploadHistory || [] : [];
  if (hist.length < 2 && upload.length < 2) return;
  const n = 120;
  const pts = (hist.length ? hist : [0,0]).slice(-n).map((v, i, a) => [w - ((a.length - 1 - i) * w) / (n - 1), h - (v / max) * h]);
  const acc = css("--accent");
  const grad = g.createLinearGradient(0, 0, 0, h);
  grad.addColorStop(0, acc + "55");
  grad.addColorStop(1, acc + "05");
  g.beginPath();
  g.moveTo(pts[0][0], h);
  pts.forEach(([x, y]) => g.lineTo(x, y));
  g.lineTo(pts[pts.length - 1][0], h);
  g.closePath();
  g.fillStyle = grad;
  g.fill();
  g.beginPath();
  pts.forEach(([x, y], i) => (i ? g.lineTo(x, y) : g.moveTo(x, y)));
  g.strokeStyle = acc;
  g.lineWidth = 1.6;
  g.stroke();
  if (upload.length > 1) {
    g.beginPath(); upload.slice(-n).forEach((v,i,a) => { const x = w - ((a.length-1-i)*w)/(n-1), y = h-(v/max)*h; if (i) g.lineTo(x,y); else g.moveTo(x,y); });
    g.strokeStyle = css("--upload"); g.stroke();
  }
}

function miniTable(table, grid, cols, rows) {
  const key = grid + cols.map((c) => c[0]).join();
  if (table._key !== key) {
    table._key = key;
    const cg = el("colgroup");
    const tr = el("tr");
    for (const [name, w] of cols) {
      const width = S["w" + grid]?.[name] || w;
      const col = el("col", width ? { style: { width: width + "px" } } : {}); // no width: takes the space left over (the file tree saves widths as "Files:", so 2.2 widths don't squeeze it)
      cg.append(col);
      const th = el("th", { text: name, style: { position: "sticky" } });
      if (grid === "Files" && name === "Download") {
        th.textContent = "";
        th.append(el("input", { type: "checkbox", "aria-label": "Select all files", "data-select-all": "" }));
      }
      const grip = el("span", { class: "grip", style: { position: "absolute", right: "0", top: "0", bottom: "0", width: "6px", cursor: "col-resize" } });
      grip.addEventListener("mousedown", (e) => {
        e.preventDefault();
        const x0 = e.clientX, w0 = col.getBoundingClientRect().width || width;
        const move = (ev) => { const nw = Math.max(20, w0 + ev.clientX - x0); col.style.width = nw + "px"; (S["w" + grid] ||= {})[name] = Math.round(nw); };
        const up = () => { removeEventListener("mousemove", move); removeEventListener("mouseup", up); onEnd(); saveLayout(); };
        addEventListener("mousemove", move);
        addEventListener("mouseup", up);
      });
      th.append(grip);
      tr.append(th);
    }
    table.replaceChildren(cg, el("thead", {}, tr), el("tbody"));
  }
  const tb = table.tBodies[0];
  // A dropdown being used would close under a re-render.
  if (document.activeElement?.tagName === "SELECT" && tb.contains(document.activeElement)) return;
  const html = rows.map((cells) => "<tr>" + cells.map((c) => `<td title="${esc(typeof c === "object" ? "" : c)}">${typeof c === "object" ? c.html : esc(c)}</td>`).join("") + "</tr>").join("");
  const empty = grid === "Trackers" ? "No trackers are available for this torrent." : grid === "Files" ? "No content information is available yet." : "";
  const content = html || (empty ? `<tr class="empty-row"><td colspan="${cols.length}">${empty}</td></tr>` : "");
  if (tb._h !== content) { tb.innerHTML = content; tb._h = content; }
}

// Torrent files as a folder tree (like qBittorrent): one top row for the whole torrent, folders with combined totals.
function fileTree(files, torrentName) {
  const root = { folder: true, name: "", path: "", map: new Map(), kids: [], all: [] };
  for (const f of files) {
    const parts = String(f.path).split(/[\\/]/).filter(Boolean);
    let node = root;
    for (const seg of parts.slice(0, -1)) {
      const path = node.path ? node.path + "/" + seg : seg;
      if (!node.map.has(seg)) { const kid = { folder: true, name: seg, path, map: new Map(), kids: [], all: [] }; node.map.set(seg, kid); node.kids.push(kid); }
      node = node.map.get(seg);
    }
    node.kids.push({ folder: false, name: parts[parts.length - 1] || f.path, file: f });
  }
  const collect = (n) => {
    if (!n.folder) return [n.file];
    n.all = n.kids.flatMap(collect);
    n.kids.sort((a, b) => (b.folder - a.folder) || a.name.localeCompare(b.name, undefined, { numeric: true }));
    return n.all;
  };
  collect(root);
  if (root.kids.length === 1 && root.kids[0].folder) return root.kids;
  if (files.length > 1) return [{ ...root, name: torrentName, path: "*torrent" }];
  return root.kids;
}
function fileTreeClosed(id) {
  S.fileTreeClosed ||= new Map();
  if (!S.fileTreeClosed.has(id)) S.fileTreeClosed.set(id, new Set());
  return S.fileTreeClosed.get(id);
}

function renderTorrentTab(t) {
  if (S.tab === "trackers") {
    miniTable($("#trackerTable"), "Trackers", [["Tier", 44], ["URL", 320], ["Status", 120], ["Seeds", 60], ["Leeches", 66], ["Peers", 56], ["Next announce", 104], ["Message", 260]],
      t.trackers.map((x) => [
        x.tier,
        { html: `<span title="${esc(x.url)}">${esc(x.url)}${x.mine ? ' <span class="muted">(added by you)</span>' : ""}</span>` },
        { html: `<span class="tracker-badge ${x.status === "Working" ? "st-Completed" : x.status === "Not working" ? "st-Error" : ""}">${esc(x.status)}</span>` }, x.seeds, x.leeches, x.peers, x.next, x.message,
      ]));
  } else if (S.tab === "peers") {
    const l = t.live;
    $("#peerSummary").textContent = l ? `Connected ${l.live} (${l.seeds} seeds) · seen ${l.seen} · connecting ${l.connecting} · queued ${l.queued} · dead ${l.dead}` + (l.down ? ` · ↓ ${l.down}` : "") + (l.up ? ` · ↑ ${l.up}` : "") : "Start the torrent to see its peers.";
    if (l?.holepunchActive) $("#peerSummary").textContent += ` · hole punching: ${l.holepunch}`;
    if (l?.turnover) $("#peerSummary").textContent += ` · ${l.turnover} idle peers swapped out`;
    $("#peerSummary").title = l?.holepunch ? "Hole punching (BEP 55) reaches peers behind NAT through a peer you both know: " + l.holepunch : "";
    miniTable($("#peerTable"), "PeerOverview", [["Peer", 120], ["Has", 110], ["Down", 74], ["Up", 74]],
      t.peers.map(p => [{ html: '<span title="' + esc(p.address + ' ' + p.flags) + '">' + esc(p.client || p.address) + '</span>' },
        { html: '<div class="bar" title="' + esc(p.progress) + '"><i style="width:' + Math.max(0,Math.min(100,parseFloat(p.progress) || 0)) + '%"></i></div>' }, p.down || "0", p.up || "0"]));
    miniTable($("#peerFullTable"), "Peers", [["IP/Address", 190], ["Connection", 70], ["Client", 160], ["Progress", 74], ["Down speed", 92], ["Up speed", 92], ["Downloaded", 96], ["Uploaded", 90], ["Flags", 220]],
      t.peers.map((p) => [p.address, p.kind || "", p.client || "", p.progress || "", p.down || "", p.up || "", p.downloaded || "", p.uploaded || "", p.flags || ""]));
  } else if (S.tab === "content") {
    const table = $("#fileTable");
    const prios = ["Don't download", "Low", "Normal", "High", "Maximum"];
    const tree = fileTree(t.files, S.rows.get(S.cur)?.name || "Torrent");
    const closed = fileTreeClosed(S.cur);
    const prioSelect = (attr, value, level) => `<select class="prio" ${attr}="${esc(value)}">${level < 0 ? '<option value="-1" selected>Mixed</option>' : ""}${prios.map((p, i) => `<option value="${i}" ${i === level ? "selected" : ""}>${p}</option>`).join("")}</select>`;
    const progress = (pct) => ({ html: `<div class="cell-prog"><span class="pt">${pct.toFixed(1)}%</span><div class="bar ${pct >= 100 ? "done" : ""}"><i style="width:${pct}%"></i></div></div>` });
    const name = (depth, html) => ({ html: `<span class="tree-name" style="--depth:${depth}">${html}</span>` });
    const rows = [];
    S.fileFolders = new Map();
    const walk = (node, depth) => {
      if (node.folder) {
        const files = node.all, size = files.reduce((a, f) => a + (f.size || 0), 0), done = files.reduce((a, f) => a + (f.done || 0), 0);
        const left = files.filter((f) => f.wanted).reduce((a, f) => a + Math.max(0, (f.size || 0) - (f.done || 0)), 0);
        const levels = new Set(files.map((f) => f.priority)), all = files.every((f) => f.wanted), some = files.some((f) => f.wanted);
        const isClosed = closed.has(node.path);
        S.fileFolders.set(node.path, files.map((f) => f.index));
        rows.push([
          { html: `<input type="checkbox" data-folder="${esc(node.path)}" ${all ? "checked" : ""} ${some && !all ? "data-mixed" : ""}>` },
          name(depth, `<button class="tree-toggle" data-toggle="${esc(node.path)}" aria-expanded="${!isClosed}" title="${isClosed ? "Expand" : "Collapse"}">${icon(isClosed ? "chevRight" : "chev")}</button>${icon("folder")}<span>${esc(node.name)}</span>`),
          fmtSize(size), progress(size ? done * 100 / size : 100),
          { html: prioSelect("data-folder-prio", node.path, levels.size === 1 ? [...levels][0] : -1) },
          left ? fmtSize(left) : (some ? "0 B" : ""),
        ]);
        if (isClosed) return;
        for (const kid of node.kids) walk(kid, depth + 1);
        return;
      }
      const f = node.file;
      rows.push([
        { html: `<input type="checkbox" data-file="${f.index}" ${f.wanted ? "checked" : ""}>` },
        name(depth, `<span class="tree-pad"></span>${icon("file")}<span title="${esc(f.path)}">${esc(node.name)}</span>`),
        f.sizeText, progress(f.progress), { html: prioSelect("data-prio", f.index, f.priority) }, f.remaining,
      ]);
    };
    for (const n of tree) walk(n, 0);
    miniTable(table, "Files", [["Download", 30], ["Name", 0], ["Size", 78], ["Progress", 112], ["Priority", 122], ["Remaining", 84]], rows);
    table.querySelectorAll("[data-folder]").forEach((c) => { c.indeterminate = c.hasAttribute("data-mixed"); });
    const selectAll = table.querySelector("[data-select-all]");
    selectAll.checked = t.files.length > 0 && t.files.every(f => f.wanted);
    selectAll.indeterminate = t.files.some(f => f.wanted) && !selectAll.checked;
    selectAll.disabled = t.files.length === 0;
    selectAll.onchange = async () => {
      const id = S.cur, wanted = selectAll.checked;
      selectAll.disabled = true;
      try {
        for (const f of t.files) {
          if (f.wanted !== wanted) await invoke("torrent_file", { id, index: f.index, wanted });
        }
      } finally {
        selectAll.disabled = false;
        refreshDetails();
      }
    };
  }
}

// ---------------------------------------------------------------- dialogs: settings

async function openSettings() {
  const g = await invoke("get_settings");
  const originalSettings = structuredClone(g.settings);
  const s = { BackgroundColor: "navy", AccentColor: "#e9c98f", AccentFromArt: true, Glass: "ghost", LineGlow: 70, ArtBehindWindow: true, ArtSelectedOnly: true, ...Object.fromEntries(Object.entries(structuredClone(g.settings)).filter(([,v]) => v !== undefined)) };
  let saved = false;
  if (s.TorrentEnableUtp === undefined) s.TorrentEnableUtp = true;
  if (!(s.TorrentMaxConnectionsPerTorrent > 0)) s.TorrentMaxConnectionsPerTorrent = 100;
  const formats = (await invoke("init")).formats;
  const f = {};
  let fieldId = 0;
  const text = (key, attrs = {}) => (f[key] = el("input", { type: "text", value: s[key] ?? "", ...attrs }));
  const num = (key, w = 80) => (f[key] = el("input", { type: "number", value: s[key] ?? 0, style: { width: w + "px" } }));
  const chk = (key, label, title) => { const i = el("input", { type: "checkbox", role: "switch" }); i.checked = !!s[key]; f[key] = i; return el("label", { class: "check", title }, i, el("span", { text: label })); };
  const speedBox = (key, allowNone = false) => {
    const i = el("input", { type: "text", "aria-label": "Speed limit" });
    const box = el("div", { class: "speedbox" }, i, el("button", { type: "button", html: icon("chev"), "aria-label": "Choose speed limit" }));
    i.dataset.none = allowNone ? "1" : "";
    f[key] = i;
    speedCombo(box, { get: () => s[key] || 0, set: (v) => { s[key] = v; }, allowNone, presets: allowNone ? UPLOAD_PRESETS : LIMIT_PRESETS });
    return box;
  };
  const field = (label, ...ctl) => {
    const control = ctl.find((c) => c.matches?.("input, select, textarea")) || ctl.map((c) => c.querySelector?.("input, select, textarea")).find(Boolean);
    if (control) { control.id ||= `settings-field-${++fieldId}`; control.setAttribute("aria-label", label); }
    return el("div", { class: "field" }, el("label", { for: control?.id, text: label }),
      el("div", { class: "field-control" }, el("div", { class: "row wrap" }, ...ctl.filter((c) => !c.classList?.contains("hint"))),
        ...ctl.filter((c) => c.classList?.contains("hint"))));
  };
  const hint = (t) => el("div", { class: "hint", style: { margin: "4px 0 8px" }, text: t });
  const section = (t) => el("h3", { class: "section", text: t });

  const selectSetting = (key, choices, fallback) => {
    const i = el("select", {}, ...choices.map(([value, label]) => el("option", { value, text: label })));
    i.value = s[key] ?? fallback; f[key] = i; return i;
  };
  const speedDays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].map((day, i) => {
    const input = el("input", { type: "checkbox", "aria-label": day }); input.checked = !!((s.ScheduleDaysMask ?? 127) & (1 << i));
    f["speedDay" + i] = input; return el("label", { class: "check" }, input, day);
  });
  const browse = el("button", { text: "Browse…", onclick: async () => { const d = await T.dialog.open({ directory: true, defaultPath: f.DefaultFolder.value }); if (d) f.DefaultFolder.value = d; } });
  const conn = el("input", { type: "range", min: 1, max: 64, value: s.DefaultConnections, class: "grow" });
  const connVal = el("b", { text: s.DefaultConnections, style: { width: "26px", textAlign: "right" } });
  conn.addEventListener("input", () => (connVal.textContent = conn.value));

  const bm = await invoke("bookmarklets", { port: null });
  const bmBox = (v) => el("input", { type: "text", readonly: true, class: "mono grow", value: v });
  const sendBm = bmBox(bm.send), grabBm = bmBox(bm.grab);

  const ext = await invoke("extension_info");
  const extButton = (b) => el("button", { text: "Add to " + b.name, onclick: () => invoke("install_extension", { exe: b.exe }).catch((e) => alertBox("Extension", String(e))) });
  const geckoButtons = ext.browsers.filter((b) => b.gecko).map(extButton), chromiumButtons = ext.browsers.filter((b) => !b.gecko).map(extButton);

  const fmtSel = el("select", {}, ...formats.map((x) => el("option", { value: x.key, text: x.label })));
  fmtSel.value = s.DefaultVideoFormat;
  const cookieSel = el("select", {}, ...["Auto", "None", "Waterfox", "Firefox", "Chrome", "Edge", "Brave", "Opera", "Vivaldi"].map((x) => el("option", { value: x, text: x === "Auto" ? "Auto (Waterfox, else Firefox)" : x })));
  cookieSel.value = s.VideoCookiesBrowser || "Auto";
  const discordStatus = el("div", { class: "hint", style: { margin: "0 0 8px" } });
  const discordTest = el("button", { text: "Send test message", onclick: async () => {
    discordTest.disabled = true; discordStatus.className = "hint"; discordStatus.textContent = "Sending…";
    try { discordStatus.textContent = await invoke("discord_test", { url: f.DiscordWebhook.value.trim() }); discordStatus.className = "hint ok-text"; }
    catch (e) { discordStatus.textContent = String(e); discordStatus.className = "hint err-text"; }
    discordTest.disabled = false;
  } });
  const ytStatus = el("span", { class: "hint grow", text: "Checking video tools…" });
  invoke("yt_version").then((v) => (ytStatus.textContent = v));
  const ytBtn = el("button", { text: "Update video tools", onclick: async () => { ytBtn.disabled = true; ytStatus.textContent = "Updating…"; ytStatus.textContent = await invoke("yt_update"); ytBtn.disabled = false; } });

  const fwText = (kind, span) => invoke("firewall_status", { kind }).then((ok) => {
    span.innerHTML = ok == null ? "" : ok ? icon("check") + " Allowed through Windows Firewall (private networks)" : "Windows Firewall may block incoming connections.";
    span.className = "hint grow " + (ok ? "ok-text" : ok === false ? "err-text" : "");
  });
  const tFw = el("span", { class: "hint grow" }), rFw = el("span", { class: "hint grow" });
  fwText("torrent", tFw);
  fwText("remote", rFw);
  const fwBtn = (kind, span) => el("button", { text: "Allow through firewall", title: "Adds a Windows Firewall rule for FluxDM on private networks only (asks for permission)", onclick: async () => {
    try { await invoke("firewall_allow", { kind }); } catch (e) { alertBox("Firewall", String(e)); }
    fwText(kind, span);
  } });

  const magStatus = el("span", { class: "hint grow" });
  const magBtn = el("button");
  const showMag = (m) => {
    magStatus.textContent = m.ours ? "FluxDM opens magnet: links." : `Magnet links currently open in: ${m.name || "nothing"}.`;
    magBtn.textContent = m.ours ? "Give magnet links back" : "Make FluxDM the magnet handler";
  };
  showMag(g.magnet);
  magBtn.addEventListener("click", async () => {
    try {
      const m = await invoke("magnet_toggle");
      showMag(m);
      if (m.needsSettings) alertBox("Magnet links", m.turningOn
        ? "Windows still has another app set as the default for magnet: links. In the Default apps page that just opened, choose FluxDM for MAGNET."
        : "FluxDM was chosen for magnet: links in Windows' Default apps, so Windows keeps using it. In the Default apps page that just opened, choose another app for MAGNET.");
    } catch (e) { alertBox("Magnet links", String(e)); }
  });

  const randomPort = el("button", { text: "Random", title: "Picks a new random port, like qBittorrent's Random button", onclick: () => (f.TorrentPort.value = 10000 + Math.floor(Math.random() * 50000)) });
  const newPin = el("button", { text: "New PIN", onclick: async () => (f.RemotePin.value = await invoke("new_pin")) });
  const pair = el("button", { text: "Pair phone (QR code)…", onclick: async () => { try { await invoke("pair_phone"); } catch (e) { alertBox("Pair phone", String(e)); } } });

  const updStatus = el("span", { class: "hint grow" });
  const updBtn = el("button", { text: "Check now", onclick: async () => {
    updStatus.textContent = "Checking…";
    try {
      const rel = await invoke("check_update", { repo: f.UpdateRepo.value.trim() });
      if (rel) { S.update = rel; $("#updateBadge").hidden = false; $("#btnUpdate").textContent = `Update to ${rel.version}`; updStatus.textContent = `Version ${rel.version} is available.`; }
      else updStatus.textContent = "You have the latest version.";
    } catch (e) { updStatus.textContent = String(e); }
  } });

  const appearanceKeys = ["BackgroundColor", "AccentColor", "AccentFromArt", "Glass", "LineGlow", "ArtBehindWindow", "LookupCoversOnline", "ArtSelectedOnly", "ShowIcons", "IconTheme", "ArtStrength", "ArtBlur", "CardArtStrength", "CardArtBlur", "BannerStrength", "BannerBlur"];
  const numericKeys = ["LineGlow", "ArtStrength", "ArtBlur", "CardArtStrength", "CardArtBlur", "BannerStrength", "BannerBlur"];
  function preview() {
    const appearance = Object.fromEntries(appearanceKeys.filter(k => f[k]).map(k => [k, f[k].type === "checkbox" ? f[k].checked : numericKeys.includes(k) ? +f[k].value : f[k].value]));
    S.settings = { ...S.settings, ...appearance };
    applyAppearance(S.settings); artKey = ""; updateWindowArt(); rebuildAllRows();
  }
  const choices = (key, values, swatches = false) => {
    f[key] = el("input", { type: "hidden", value: s[key] });
    const box = el("div", { class: "appearance-choices", role: "group", "aria-label": key });
    for (const [value, label, colour] of values) {
      const button = el("button", { type: "button", "aria-label": label, title: label,
        class: swatches ? "accent-swatch" : "", style: swatches ? { background: value } : {},
        onclick: () => { f[key].value = value; sync(); preview(); } },
        colour ? el("span", { class: "background-swatch", style: { background: colour } }) : null, swatches ? null : label);
      button.dataset.value = value; box.append(button);
    }
    function sync() { for (const button of box.children) button.setAttribute("aria-pressed", String(button.dataset.value === f[key].value)); }
    sync(); return el("div", {}, f[key], box);
  };
  // The five icon themes, each shown as the icon itself.
  const iconThemes = () => {
    f.IconTheme = el("input", { type: "hidden", value: s.IconTheme || "heat" });
    const box = el("div", { class: "appearance-choices icon-themes", role: "group", "aria-label": "App icon" });
    for (const [id, label] of ICON_THEMES) {
      const pic = el("img", { alt: "", src: "icon.png" });
      themedLogo(id).then((url) => { pic.src = url; });
      const b = el("button", { type: "button", title: label, "aria-label": label, onclick: () => { f.IconTheme.value = id; sync(); preview(); } }, pic, el("span", { text: label }));
      b.dataset.value = id;
      box.append(b);
    }
    function sync() { for (const b of box.children) b.setAttribute("aria-pressed", String(b.dataset.value === f.IconTheme.value)); }
    sync();
    return el("div", {}, f.IconTheme, box);
  };  const glow = f.LineGlow = el("input", { type: "range", min: 20, max: 100, value: s.LineGlow, "aria-label": "Progress line glow" });
  const glowValue = el("output", { text: s.LineGlow + "%", class: "mono" });
  glow.addEventListener("input", () => { glowValue.textContent = glow.value + "%"; });
  const artStrength = f.ArtStrength = el("input", { type: "range", min: 25, max: 500, step: 25, value: s.ArtStrength || 100, "aria-label": "Background art strength" });
  const artStrengthValue = el("output", { text: (s.ArtStrength || 100) + "%", class: "mono" });
  artStrength.addEventListener("input", () => { artStrengthValue.textContent = artStrength.value + "%"; });
  const bannerStrength = f.BannerStrength = el("input", { type: "range", min: 25, max: 150, step: 5, value: s.BannerStrength || 100, "aria-label": "Header banner strength" });
  const bannerStrengthValue = el("output", { text: (s.BannerStrength || 100) + "%", class: "mono" });
  bannerStrength.addEventListener("input", () => { bannerStrengthValue.textContent = bannerStrength.value + "%"; });
  const cardArtStrength = f.CardArtStrength = el("input", { type: "range", min: 25, max: 400, step: 25, value: s.CardArtStrength || 100, "aria-label": "Selected download art strength" });
  const cardArtStrengthValue = el("output", { text: (s.CardArtStrength || 100) + "%", class: "mono" });
  cardArtStrength.addEventListener("input", () => { cardArtStrengthValue.textContent = cardArtStrength.value + "%"; });
  const body = el("div", {},
    section("Appearance"), hint("How FluxDM looks. Changes show right away."),
    field("Background colour", choices("BackgroundColor", [["navy","Deep navy","#0b1222"],["black","Pure black","#000000"],["graphite","Graphite","#16181c"],["purple","Purple night","#150f22"],["forest","Forest","#0b1a14"]])),
    field("Accent colour", el("div", {}, chk("AccentFromArt", "Take it from the game's art when there is one"), hint("Otherwise"), choices("AccentColor", [["#e9c98f","Warm cream"],["#f5a524","Amber"],["#4cc2ff","Cyan"],["#a78bfa","Violet"],["#5eead4","Mint"]], true))),
    field("App icon", iconThemes()),
    hint("For the tray icon, the taskbar and window icons and the logo in the app. The FluxDM.exe file keeps Full heat in Explorer and the Start menu."),
    field("Glass", choices("Glass", ["Solid","Frosted","Clear","Ghost"].map(label => [label.toLowerCase(),label]))),
    field("Progress line glow", glow, glowValue),
    field("Background art strength", artStrength, artStrengthValue),
    field("Background art blur", selectSetting("ArtBlur", [["-1", "Automatic"], ["0", "None (sharp)"], ["8", "Light"], ["20", "Medium"], ["40", "Strong"], ["70", "Very strong"]], "-1")),
    field("Header banner strength", bannerStrength, bannerStrengthValue),
    field("Header banner blur", selectSetting("BannerBlur", [["0", "None (sharp)"], ["3", "Light"], ["8", "Medium"], ["16", "Strong"]], "0")),
    field("Selected download art strength", cardArtStrength, cardArtStrengthValue),
    field("Selected download art blur", selectSetting("CardArtBlur", [["0", "None (sharp)"], ["3", "Light"], ["8", "Medium"], ["16", "Strong"]], "0")),
    hint("How much of the game art shows behind the window. 100% keeps it faint so text stays easy to read; raise it to see more of the picture. The “Header banner” options set the big picture above the details; the “Selected download” ones the art on the highlighted download in the list. The “Clear” or “Ghost” glass lets the most art through."),
    field("Game art", el("div", {}, chk("ArtBehindWindow", "Show game art behind the window"), chk("LookupCoversOnline", "Look up game banners and icons online"), chk("ArtSelectedOnly", "Show art in the list only for the selected download"), chk("ShowIcons", "Show a picture before each download's name"))),
    section("Downloads"),
    field("Default save folder", text("DefaultFolder", { class: "mono grow" }), browse),
    field("When a file already exists", selectSetting("ExistingFilePolicy", [["Ask", "Ask"], ["Number", "Add a number"], ["Overwrite", "Overwrite"], ["Skip", "Skip"]], "Number")),
    hint("Applies to regular file downloads. Torrents verify existing data and video tools retain their own file handling."),
    field("Folder for unfinished downloads", text("IncompleteFolder", { class: "mono grow" }), el("button", { text: "Browse…", onclick: async () => { const d = await T.dialog.open({ directory: true }); if (d) f.IncompleteFolder.value = d; } })),
    hint("Optional. New file and torrent downloads use this folder, then move to their destination when finished. Leave blank to use the destination throughout."),
    field("Proxy", selectSetting("ProxyMode", [["None", "None"], ["System", "System"], ["HTTP", "HTTP"], ["SOCKS5", "SOCKS5"]], "System")),
    field("Proxy host", text("ProxyHost"), num("ProxyPort")),
    field("Proxy login (optional)", text("ProxyUsername", { autocomplete: "off" }), text("ProxyPassword", { type: "password", autocomplete: "off" })),
    field("Local addresses", chk("ProxyBypassLocal", "Don't use the proxy for local addresses")),
    hint("System uses proxy environment variables or the Windows manual proxy; automatic PAC scripts are not supported. HTTP downloads, metadata and covers use the proxy. SOCKS5 also routes outgoing torrent TCP peers; uTP is disabled. DHT and UDP trackers still connect directly. Torrent proxy changes take effect after restarting FluxDM."),
    field("Sub-folders", chk("CategorySubfolders", "Sort into sub-folders by type (Video, Music, Programs, AI Models…)")),
    field("Manual downloads at the same time", num("MaxManualDownloads")),
    field("Connections per download", conn, connVal, el("span", { class: "hint", text: "8–16 works well for most websites." })),
    field("Global speed limit", speedBox("GlobalSpeedLimitKBps"), el("span", { class: "hint", text: "e.g. 750k, 2 MB/s — 0 = unlimited" })),

    section("Speed schedule"),
    field("Speed schedule", chk("SpeedScheduleEnabled", "Limit the speed during part of the day")),
    field("Hours (24-hour time)", text("ScheduleFrom", { style: { width: "70px" } }), "to", text("ScheduleTo", { style: { width: "70px" } })),
    field("Days", ...speedDays),
    field("Scheduled speed limit", speedBox("ScheduleLimitKBps")),
    hint("Outside these hours the normal global limit applies. Overnight hours belong to the selected starting day. The window may cross midnight (e.g. 23:00 to 07:00). If both limits apply, the stricter one wins."),

    section("Reliability"),
    field("Retries per connection", num("MaxRetries")),
    field("Reconnect a stalled connection after (s)", num("ReadTimeoutSec")),
    hint("Retry failed downloads can be enabled for each queue in Scheduler."),

    section("Browser integration"),
    field("Clipboard", chk("ClipboardMonitor", "Watch the clipboard — copying a download link opens the Add dialog")),
    field("File types to catch", text("ClipboardExtensions", { class: "grow" })),
    field("Browser extension", chk("LocalApiEnabled", "Allow the browser extension to send downloads here")),
    field("Browser integration port", num("LocalApiPort")),
    hint("Browser bookmarklets: create a new bookmark and paste one of these as its URL. They only open FluxDM's Add dialog — nothing downloads without your click. Only programs on this PC can reach the bridge."),
    el("b", { text: "“Send to FluxDM” — sends the current page/file URL" }),
    el("div", { class: "row", style: { margin: "4px 0 8px" } }, sendBm, el("button", { text: "Copy", onclick: () => invoke("copy_text", { text: sendBm.value }) })),
    el("b", { text: "“Grab links” — collects every downloadable link on the page" }),
    el("div", { class: "row", style: { margin: "4px 0 8px" } }, grabBm, el("button", { text: "Copy", onclick: () => invoke("copy_text", { text: grabBm.value }) })),

    section("Browser extension"),
    hint("Catches download buttons and links in your browser and sends them here (needs the browser bridge above). Works in Firefox/Waterfox and Chrome/Edge/Brave/Opera/Vivaldi."),
    ext.signed
      ? el("p", { html: "<b>Firefox / Waterfox:</b> click its button, then <b>Add</b> in the browser's prompt." })
      : el("p", { html: "<b>Waterfox / Firefox Developer / ESR:</b> open about:config, set <code>xpinstall.signatures.required</code> to false, then click its button (or about:addons → gear menu → Install Add-on From File… and pick <b>FluxDM-extension.xpi</b>). Regular Firefox needs the signed build." }),
    geckoButtons.length ? el("div", { class: "row wrap", style: { margin: "4px 0 8px" } }, ...geckoButtons) : hint("No Firefox-family browser found on this PC."),
    el("p", { html: "<b>Chrome / Edge / Brave / Opera / Vivaldi:</b> click its button. It opens the browser's extensions page and the extension folder: turn on <b>Developer mode</b>, click <b>Load unpacked</b> and pick the <b>BrowserExtension</b> folder that just opened." }),
    chromiumButtons.length ? el("div", { class: "row wrap", style: { margin: "4px 0 8px" } }, ...chromiumButtons) : hint("No Chromium browser found on this PC."),
    el("button", { text: "Open extension folder", onclick: () => invoke("open_extension_folder").catch((e) => alertBox("Extension", String(e))) }),

    section("Videos and audio"),
    hint("For YouTube and 1000+ other sites: tick “Download as video” in the Add window, or right-click a page → Download video with FluxDM. yt-dlp and ffmpeg are fetched once from their official releases on first use and kept in FluxDM's data folder."),
    field("Default quality", fmtSel),
    field("Use browser cookies from", cookieSel, el("span", { class: "hint", text: "Needed for private or age-restricted videos. Close Chrome or Edge first." })),
    field("Or a cookies.txt file", text("VideoCookiesFile", { class: "mono grow", placeholder: "Optional: replaces the browser cookies" }),
      el("button", { text: "Browse…", onclick: async () => { const p = await T.dialog.open({ filters: [{ name: "cookies.txt", extensions: ["txt"] }] }); if (p) f.VideoCookiesFile.value = p; } })),
    field("Custom format", text("VideoCustomFormat", { class: "mono grow", placeholder: "yt-dlp -f selector, e.g. bv*[vcodec^=avc1]+ba/b" })),
    hint("Used when the quality is “Custom format”. Leave it alone unless you know yt-dlp's format syntax."),
    field("Subtitles", selectSetting("VideoSubtitles", [["None", "Don't download"], ["Download", "Download as separate files"], ["Embed", "Put them inside the video"]], "None"),
      text("VideoSubLangs", { class: "mono", style: { width: "140px" }, title: "Languages, comma-separated (en.*,ar.* = all English and Arabic variants)" })),
    field("Auto-generated subtitles", chk("VideoAutoSubs", "Also get the site's automatic captions when there are no real ones")),
    field("Inside the file", chk("VideoEmbedMeta", "Add the thumbnail, title/artist details and chapters")),
    field("SponsorBlock (YouTube)", selectSetting("VideoSponsorblock", [["Off", "Off"], ["Mark", "Mark sponsor segments as chapters"], ["Remove", "Cut sponsor segments out"]], "Off")),
    hint("SponsorBlock covers sponsors, self-promotion, subscribe reminders, intros and outros, from the community's SponsorBlock list."),
    field("Live streams", chk("VideoLiveFromStart", "Record a live stream from its start, not from now"), chk("VideoWaitForLive", "Wait for a scheduled stream to begin")),
    field("Duplicates", chk("VideoUseArchive", "Don't download the same video twice (remembers downloaded videos)")),
    el("div", { class: "row" }, ytStatus, ytBtn),

    section("Torrents"),
    hint("Paste a magnet link or a link to a .torrent file into Add URL, or use “Open .torrent…” there to add one from disk. Torrents get FluxDM's speed limits, queues, stats and phone control like any other download."),
    field("Listening port (peers + DHT)", num("TorrentPort"), randomPort, el("span", { class: "hint", text: "0 = pick a free one" })),
    field("Router", chk("TorrentUseUPnP", "Use UPnP port forwarding from my router")),
    field("µTP", chk("TorrentEnableUtp", "Also connect over µTP (UDP), like qBittorrent — reaches more peers (applies after restarting FluxDM)", "µTP peers show “µTP” in the Peers tab. Turn this off if torrents misbehave.")),
    field("Maximum connections per torrent", num("TorrentMaxConnectionsPerTorrent"), el("span", { class: "hint", text: "applies after restarting FluxDM" })),
    field("Upload limit while downloading", speedBox("TorrentUploadLimitKBps", true), el("span", { class: "hint", text: "e.g. 50k, 2 MB/s — 0 = unlimited, none = no upload" })),
    field("Seeding limit after it finishes", speedBox("TorrentSeedingLimitKBps", true), el("span", { class: "hint", text: "sharing finished torrents — 0 = unlimited, none = off" })),
    hint("Each is a total shared by all torrents in that state. Right-click a torrent to give it its own limits too — the stricter one wins."),
    field("Seeding time limit (minutes)", num("TorrentSeedMinutes"), el("span", { class: "hint", text: "0 = off; counts time spent sharing, including across restarts" })),
    field("When a seeding limit is reached", selectSetting("TorrentSeedAction", [["Stop", "Stop sharing"], ["Remove", "Remove torrent (keep files)"]], "Stop")),
    field("Always add these trackers", (() => { const i = el("textarea", { rows: 4, class: "mono grow", value: s.TorrentAlwaysTrackers || "", placeholder: "One HTTP, HTTPS or UDP tracker URL per line" }); f.TorrentAlwaysTrackers = i; return i; })()),
    field("Don't download files matching", (() => { const i = el("textarea", { rows: 4, class: "mono grow", value: s.TorrentSkipPatterns || "", placeholder: "*.txt\n*.url\n*sample*" }); f.TorrentSkipPatterns = i; return i; })()),
    field("Unfinished files", chk("TorrentMarkUnfinished", "Add .!fx until each file finishes")),
    field("Watched folder", text("TorrentWatchFolder", { class: "mono grow", placeholder: "Optional folder for .torrent files" })),
    hint("Stable .torrent files are added after a few seconds using the default folder/category and moved into an added subfolder. Invalid files stay in place with a warning. Clear the folder to stop watching."),
    field("IP blocklist", text("TorrentBlocklistUrl", { class: "mono grow", placeholder: "HTTP(S) URL or absolute local file path" })),
    hint("PeerGuardian ranges (name:start-end), plain text or gzip. Loaded when the torrent engine starts and refreshed weekly; refresh failures retain the previous list. An initial load failure stops the engine with an error. Blank = off."),
    field("Peer discovery", chk("TorrentEnableDht", "Enable DHT"), chk("TorrentEnableLocalDiscovery", "Enable local peer discovery")),
    field("IP version", chk("TorrentIpv4Only", "Use IPv4 only")),
    hint("Blocklist, discovery and IP version changes apply after restarting FluxDM."),
    hint("Trackers, filename patterns and unfinished marking apply to new torrents. Matching files start unticked and can be selected in Add or Files. Patterns match filenames without the storage extension, ignoring case; * matches any text and ? one character."),
    field("Keep sharing until ratio", num("TorrentSeedRatio"), el("span", { class: "hint", text: "uploaded ÷ downloaded — 0 = stop seeding right away" })),
    el("div", { class: "row", style: { marginTop: "8px" } }, tFw, fwBtn("torrent", tFw)),
    hint("If this is blocked, other peers can't connect to you — you'll only ever reach the smaller set you happen to connect out to, which usually means fewer peers and slower downloads."),
    el("div", { class: "row", style: { marginTop: "8px" } }, magStatus, magBtn),
    hint("This decides which app opens when you click a magnet: link anywhere on Windows (e.g. in a browser). You can switch back at any time."),

    section("Phone remote"),
    field("Phone remote", chk("RemoteEnabled", "Control FluxDM from my phone on the home Wi-Fi")),
    field("Phone connection port", num("RemotePort")),
    field("Phone PIN", text("RemotePin", { class: "mono", style: { width: "100px" } }), newPin, pair),
    el("div", { class: "row", style: { marginLeft: "23px", marginTop: "6px" } }, rFw, fwBtn("remote", rFw)),
    hint("Your phone opens a web page served by this PC — no app or account needed. Anyone on your network also needs the PIN; 10 wrong PINs lock that device out for 5 minutes."),

    section("Discord notifications"),
    hint("Get messages in your own Discord channel. In Discord: your server → channel settings → Integrations → Webhooks → New Webhook → Copy Webhook URL, then paste it here. Keep the link private: anyone who has it can post in that channel."),
    field("Webhook link", text("DiscordWebhook", { type: "password", class: "mono grow", autocomplete: "off", placeholder: "https://discord.com/api/webhooks/…" }), discordTest),
    discordStatus,
    field("Send a message when", el("div", {},
      chk("DiscordOnFinished", "A download finished"),
      chk("DiscordOnFailed", "A download failed"),
      chk("DiscordOnQueue", "A queue finished"),
      el("div", { class: "row" }, chk("DiscordOnDisk", "The disk is almost full: under"), num("DiscordDiskGB", 70), el("span", { text: "GB free" })),
      el("div", { class: "row" }, chk("DiscordDaily", "A daily summary at"), text("DiscordDailyTime", { style: { width: "70px" } })))),
    hint("All are off until you tick them. If Discord can't be reached, downloads carry on; the message is just skipped."),

    section("Behaviour"),
    field("Tray", chk("CloseToTray", "Closing the window keeps FluxDM running in the tray")),
    field("Sleep", chk("PreventSleep", "Keep the PC awake while downloads are running")),
    field("Notifications", chk("NotifyOnComplete", "Notify me when a download or queue finishes")),
    field("Startup", chk("StartWithWindows", "Start with Windows (in the tray)")),

    section("Updates"),
    field("Automatic check", chk("AutoCheckUpdates", "Check for a new version when FluxDM starts")),
    el("div", { class: "row" }, updStatus, updBtn),
    section("Advanced"),
    field("User-Agent", text("UserAgent", { class: "mono grow" })),
    field("Update source (GitHub owner/repo)", text("UpdateRepo", { class: "mono grow", placeholder: "owner/FluxDM" })),
    el("div", { class: "hint", style: { marginTop: "14px" }, text: `FluxDM ${(await invoke("init")).version} · data folder: ${g.dataDir}` }),
  );

  function collect() {
    const out = structuredClone(s);
    for (const [k, i] of Object.entries(f)) {
      if (k.startsWith("speedDay")) continue;
      if (i.type === "checkbox") out[k] = i.checked;
      else if (i.type === "number") out[k] = Number(i.value) || 0;
      else if (["GlobalSpeedLimitKBps", "ScheduleLimitKBps", "TorrentUploadLimitKBps", "TorrentSeedingLimitKBps"].includes(k)) {
        const v = parseSpeed(i.value, !!i.dataset.none);
        if (v == null) { i.classList.add("invalid"); i.focus(); throw new Error("Couldn't read that speed. Try 750k or 2 MB/s."); }
        out[k] = v;
      } else out[k] = i.value;
    }
    out.ScheduleDaysMask = speedDays.reduce((mask, label, i) => mask | (f["speedDay" + i].checked ? 1 << i : 0), 0);
    out.LineGlow = +glow.value;
    if (f.ArtStrength) out.ArtStrength = +f.ArtStrength.value;
    if (f.ArtBlur) out.ArtBlur = +f.ArtBlur.value;
    if (f.BannerStrength) out.BannerStrength = +f.BannerStrength.value;
    if (f.BannerBlur) out.BannerBlur = +f.BannerBlur.value;
    if (f.CardArtStrength) out.CardArtStrength = +f.CardArtStrength.value;
    if (f.CardArtBlur) out.CardArtBlur = +f.CardArtBlur.value;
    out.DefaultConnections = +conn.value;
    out.DefaultVideoFormat = fmtSel.value;
    out.VideoCookiesBrowser = cookieSel.value;
    out.TorrentSeedRatio = Number(f.TorrentSeedRatio.value) || 0;
    return out;
  }

  let group;
  for (const child of [...body.children]) {
    if (child.matches("h3.section")) {
      group = el("section", { class: "settings-group" });
      body.append(group);
    }
    group.append(child);
  }

  const groups = [...body.children];
  const search = el("input", { type: "search", placeholder: "Find a setting", "aria-label": "Find a setting" });
  const nav = el("nav", { class: "settings-nav", "aria-label": "Settings sections" }, el("div", { class: "settings-title", html: icon("gear") + "Settings" }), search);
  const heads = $$("h3.section", body);
  const descriptions = ["How FluxDM looks. Changes show right away.", "Where files go and how fast", "Slow downloads down during part of the day", "What FluxDM does when a connection breaks", "Catching downloads from the browser and clipboard", "Install the extension in your browsers", "Downloading from YouTube and other video sites", "The built-in torrent engine", "Control downloads from your phone", "How FluxDM behaves on your PC", "Keeping FluxDM up to date", "Only change these if you know you need to"];
  const appearanceHelp = ["The base colour behind the glass and game art", "Buttons, progress line and highlights", "How see-through the panels are", "Brightness of the line on the banner", "Banners and covers from Steam and console patch pages", "Optional extra game banners"];
  $$(".field", groups[0]).forEach((row,i) => row.firstChild.append(el("small", { text: appearanceHelp[i] })));
  groups[0].querySelector(".hint").remove();
  const sectionNames = heads.map(h => h.textContent);
  const glyphs = ["palette","down","clock","shield","browser","extension","video","magnet","phone","sliders","refresh","wrench"];
  heads.forEach((h,i) => {
    h.prepend(el("span", { class: "settings-page-icon", html: icon(glyphs[i]) }));
    h.append(el("small", { text: descriptions[i] }));
  });
  let current = 0;
  const empty = el("p", { class: "hint", text: "No settings match your search.", hidden: true });
  function showPage() {
    const query = search.value.trim().toLocaleLowerCase();
    let matches = 0;
    groups.forEach((page,i) => {
      let count = 0;
      for (const row of [...page.children].slice(1)) {
        row.hidden = !!query && !(heads[i].textContent + " " + row.textContent).toLocaleLowerCase().includes(query);
        if (!row.hidden) count++;
      }
      page.hidden = query ? !count : i !== current;
      if (!page.hidden) matches++;
      navButtons[i].classList.toggle("active", !query && i === current);
      navButtons[i].setAttribute("aria-current", !query && i === current ? "page" : "false");
    });
    empty.hidden = !!matches; body.scrollTop = 0;
  }
  const navButtons = heads.map((h,i) => el("button", { class: "ghost", onclick: () => { current = i; search.value = ""; showPage(); } }, el("span", { html: icon(glyphs[i]) }), sectionNames[i]));
  nav.append(...navButtons); body.append(empty);
  search.addEventListener("input", showPage);
  body.addEventListener("input", e => { if (appearanceKeys.some(k => f[k] === e.target)) preview(); });
  body.addEventListener("change", e => { if (appearanceKeys.some(k => f[k] === e.target)) preview(); });
  const wrap = el("div", { class: "settings-wrap" }, nav, body);
  body.classList.add("settings-body"); showPage();
  const fingerprint = () => JSON.stringify([...Object.values(f), conn, fmtSel, cookieSel].map(i => i.type === "checkbox" ? i.checked : i.value));
  const initial = fingerprint();

  modal({
    title: "Settings", body: wrap, xwide: true,
    beforeClose: () => saved || fingerprint() === initial || confirmBox("Unsaved changes", "Discard your unsaved settings changes?", "Discard changes", true),
    onClose: () => { if (!saved) { S.settings = { ...S.settings, ...Object.fromEntries(appearanceKeys.map(k => [k, originalSettings[k]])) }; applyAppearance(S.settings); artKey = ""; updateWindowArt(); rebuildAllRows(); } },
    buttons: [
      { label: "Open data folder", left: true, click: () => { invoke("open_data_folder"); return false; } },
      { label: "Cancel" },
      {
        label: "Save", primary: true,
        click: async () => {
          try {
            const out = collect();
            await invoke("save_settings", { settings: out });
            saved = true;
            S.settings = out;
            applyAppearance(out);
            artKey = ""; updateWindowArt(); rebuildAllRows(); refreshFreeSpace();
            showLimit();
          } catch (e) { alertBox("Settings", e.message || String(e)); return false; }
        },
      },
    ],
  });
}

// ---------------------------------------------------------------- dialogs: scheduler

function scheduleTime(value) {
  const match = /^\s*(\d{1,2})\s*(?::\s*(\d{1,2})\s*)?$/.exec(String(value || ""));
  if (!match || +match[1] > 23 || +(match[2] || 0) > 59) return null;
  return String(+match[1]).padStart(2, "0") + ":" + String(+(match[2] || 0)).padStart(2, "0");
}

function scheduleSummary(q) {
  const start = scheduleTime(q.StartAt) || q.StartAt, stop = scheduleTime(q.StopAt) || q.StopAt;
  const names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"], mask = q.DaysMask & 127;
  const frequency = !q.Daily ? `on ${String(q.OnceDate || "").slice(0, 10) || "the chosen date"}`
    : mask === 127 ? "every day" : mask === 62 ? "every weekday" : mask === 65 ? "every weekend"
    : mask ? `on ${[1, 2, 3, 4, 5, 6, 0].filter(n => mask & (1 << n)).map(n => names[n]).join(", ")}` : "on no days (choose at least one)";
  return `${q.StartAtEnabled ? `Starts ${frequency} at ${start}` : "No timed start"}${q.StopAtEnabled ? `, stops at ${stop}${q.StartAtEnabled && stop <= start ? " the next day" : ""}` : ""}${q.StartOnLaunch ? "; also starts when FluxDM opens" : ""}.`;
}

function scheduleNextStart(q, now = new Date()) {
  if (!q.StartAtEnabled) return q.StartOnLaunch ? "When FluxDM opens" : "No automatic start";
  const clock = scheduleTime(q.StartAt);
  if (!clock) return "Choose a start time";
  const time = clock.split(":").map(Number);
  const last = String(q.LastStartSlot || q.LastStartFire || "").slice(0, 10);
  const keyOf = d => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  const candidates = q.Daily ? Array.from({ length: 9 }, (_, i) => new Date(now.getFullYear(), now.getMonth(), now.getDate() + i - 1, time[0], time[1]))
    : [new Date(`${String(q.OnceDate || "").slice(0, 10)}T${clock}`)];
  for (const start of candidates) {
    if (!Number.isFinite(+start)) continue;
    if (keyOf(start) <= last || (q.Daily && !(q.DaysMask & (1 << start.getDay())))) continue;
    const stopClock = q.StopAtEnabled && scheduleTime(q.StopAt);
    const stop = stopClock && stopClock.split(":").map(Number);
    const end = stop ? new Date(start.getFullYear(), start.getMonth(), start.getDate() + (stopClock <= clock ? 1 : 0), stop[0], stop[1])
      : new Date(start.getFullYear(), start.getMonth(), start.getDate() + 1, time[0], time[1]);
    if (start <= now && now < end) return "Start due now";
    if (start <= now) continue;
    const minutes = Math.ceil((start - now) / 60000);
    return `${["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][start.getDay()]} ${clock} - in ${Math.floor(minutes / 60)} h ${minutes % 60} m`;
  }
  return q.Daily ? "No scheduled days" : "One-off start has passed";
}

async function openScheduler(focusName) {
  let queues = await invoke("queues");
  let cur = focusName || (S.nav.startsWith("q:") ? S.nav.slice(2) : queues[0]?.Name);
  let memberSel = null, dragged = null, closed = false, busy = false, refreshing = false, saving = 0;
  let saveChain = Promise.resolve(), revision = 0, listKey = "";
  const list = el("div", { class: "sched-list", "aria-label": "Queues" });
  const right = el("div", { class: "sched-editor" });
  const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
  const DAYBIT = { Sun: 0, Mon: 1, Tue: 2, Wed: 3, Thu: 4, Fri: 5, Sat: 6 };
  let members, itemButtons = [], summary;
  const selected = () => queues.find(q => q.Name === cur);

  async function reload() {
    queues = await invoke("queues");
    if (closed) return;
    if (!selected()) cur = queues[0]?.Name;
    draw();
  }
  function save(q) {
    const def = { ...q }, oldName = q._old || q.Name;
    delete q._old; delete def._old;
    saving++; revision++; updateRunButtons();
    saveChain = saveChain.then(async () => {
      try { await invoke("queue_save", { oldName, def }); }
      catch (e) {
        alertBox("Scheduler", String(e));
        try { await reload(); } catch { /* A later refresh can recover after IPC becomes available. */ }
      }
      finally { saving--; if (!closed) { updateRunButtons(); drawMembers(); } }
    });
    return saveChain;
  }
  async function action(fn) {
    if (busy || saving) return;
    busy = true; revision++; updateRunButtons(); drawMembers();
    try { await fn(); if (!closed) await reload(); }
    catch (e) { alertBox("Scheduler", String(e)); }
    finally { busy = false; if (!closed) { updateRunButtons(); drawMembers(); } }
  }
  function updateRunButtons() {
    const q = selected();
    startBtn.disabled = busy || !!saving || !q || q.Running;
    stopBtn.disabled = busy || !!saving || !q || !q.Running;
    startBtn.textContent = q?.Running ? "Queue running" : "Start queue";
    stopBtn.textContent = q?.Running ? "Stop queue" : "Queue stopped";
  }
  function drawList() {
    const key = JSON.stringify([cur, ...queues.map(q => [q.Name, q.Running, q.Members.length, scheduleNextStart(q)])]);
    if (key === listKey) return;
    listKey = key;
    list.replaceChildren(...queues.map(q => {
      const row = el("button", { type: "button", class: "sched-queue" + (q.Name === cur ? " active" : ""), "aria-pressed": String(q.Name === cur) });
      row.append(el("span", { class: "sched-dot" + (q.Running ? " running" : ""), title: q.Running ? "Running" : "Stopped", "aria-label": q.Running ? "Running" : "Stopped" }),
        el("span", { class: "sched-queue-text" }, el("b", { class: "ell", text: q.Name }), el("small", { text: scheduleNextStart(q) })),
        el("span", { class: "count", text: q.Members.length, title: `${q.Members.length} items` }));
      row.addEventListener("click", () => { if (busy || saving) return; cur = q.Name; memberSel = null; draw(); });
      return row;
    }));
  }
  function moveTo(index) {
    const q = selected(), from = q?.Members.findIndex(m => m.id === memberSel);
    if (from == null || from < 0 || index < 0 || index >= q.Members.length || from === index) return;
    return action(() => invoke("queue_move", { name: q.Name, id: memberSel, dir: index - from }));
  }
  function drawMembers() {
    const q = selected();
    if (!q || !members) return;
    if (!q.Members.some(m => m.id === memberSel)) memberSel = null;
    const focusedId = [...members.children].find(row => row === document.activeElement)?.dataset.id;
    members.replaceChildren(...(q.Members.length ? q.Members.map((m, index) => {
      const row = el("div", { class: "m" + (memberSel === m.id ? " sel" : ""), draggable: "true", tabindex: 0, role: "option", "aria-selected": String(memberSel === m.id) },
        el("span", { class: "hint", text: index + 1 }), el("span", { class: "grow ell", text: m.name, title: m.name }), el("span", { class: "hint", text: `${m.size} · ${m.status}` }));
      row.dataset.id = m.id;
      const select = () => { memberSel = m.id; drawMembers(); };
      row.addEventListener("click", select);
      row.addEventListener("keydown", e => { if (e.key === " " || e.key === "Enter") { e.preventDefault(); select(); members.children[index]?.focus(); } });
      row.addEventListener("dragstart", e => { if (busy || saving) { e.preventDefault(); return; } dragged = m.id; memberSel = m.id; e.dataTransfer.setData("text/plain", m.id); e.dataTransfer.effectAllowed = "move"; });
      row.addEventListener("dragover", e => { if (dragged && !busy && !saving) { e.preventDefault(); e.dataTransfer.dropEffect = "move"; } });
      row.addEventListener("drop", e => { if (!dragged) return; e.preventDefault(); memberSel = dragged; dragged = null; moveTo(index); });
      row.addEventListener("dragend", () => { dragged = null; });
      return row;
    }) : [el("div", { class: "m hint", text: "Nothing in this queue yet. Right-click downloads in the main list → Move to queue." })]));
    const i = q.Members.findIndex(m => m.id === memberSel);
    itemButtons.forEach((b, n) => { b.disabled = busy || !!saving || (n === 5 ? !q.Members.length : i < 0 || (n < 2 ? i === 0 : n < 4 ? i === q.Members.length - 1 : false)); });
    members.dataset.snapshot = JSON.stringify(q.Members);
    if (focusedId) [...members.children].find(row => row.dataset.id === focusedId)?.focus();
  }
  function draw() {
    drawList();
    const q = selected();
    if (!q) { right.replaceChildren(); updateRunButtons(); return; }
    const commit = () => { summary.textContent = scheduleSummary(q); drawList(); return save(q); };
    const inp = (key, attrs = {}) => {
      const i = el("input", { type: "text", value: attrs.type === "time" ? scheduleTime(q[key]) || "" : q[key], "aria-label": key, ...attrs });
      i.addEventListener("change", () => {
        if (i.checkValidity && !i.checkValidity()) { i.reportValidity(); return; }
        if (key === "OnceDate" && !i.value) return;
        if (key === "Name" && !q._old) q._old = q.Name;
        q[key] = attrs.type === "number" ? +i.value : key === "OnceDate" ? i.value + "T00:00:00" : i.value;
        if (key === "Name") { q.Name = q.Name.trim(); cur = q.Name; }
        return commit();
      }); return i;
    };
    const cb = (key, label, after) => {
      const i = el("input", { type: "checkbox" }); i.checked = !!q[key];
      i.addEventListener("change", () => { q[key] = i.checked; after?.(); return commit(); });
      return el("label", { class: "check" }, i, el("span", { text: label }));
    };
    const field = (label, control) => el("div", { class: "field" }, el("span", { text: label }), control);
    const section = (name, ...content) => el("section", { class: "sched-section" }, el("h3", { class: "section", text: name }), ...content);
    const lim = el("div", { class: "speedbox" }, el("input", { type: "text", "aria-label": "Queue speed limit" }), el("button", { type: "button", html: icon("chev"), "aria-label": "Choose speed limit" }));
    speedCombo(lim, { get: () => q.SpeedLimitKBps, set: v => { q.SpeedLimitKBps = v; commit(); } });
    const dayInputs = [];
    const days = el("div", { class: "days" }, ...DAYS.map(d => {
      const i = el("input", { type: "checkbox" }); i.checked = !!(q.DaysMask & (1 << DAYBIT[d])); dayInputs.push(i);
      i.addEventListener("change", () => { q.DaysMask = i.checked ? q.DaysMask | (1 << DAYBIT[d]) : q.DaysMask & ~(1 << DAYBIT[d]); return commit(); });
      return el("label", {}, i, d);
    }));
    const shortcuts = el("div", { class: "row sched-actions" }, ...[["Every day", 127], ["Weekdays", 62], ["Weekends", 65]].map(([text, mask]) => el("button", { type: "button", class: "small", text, onclick: () => { q.DaysMask = mask; dayInputs.forEach((i, n) => { i.checked = !!(mask & (1 << DAYBIT[DAYS[n]])); }); return commit(); } })));
    const repeat = el("div", {}, days, shortcuts);
    const once = inp("OnceDate", { type: "date", value: String(q.OnceDate || "").slice(0, 10), "aria-label": "One-off date" });
    const mode = el("select", { "aria-label": "Schedule repetition" }, el("option", { value: "daily", text: "Repeat on days" }), el("option", { value: "once", text: "One-off date" }));
    mode.value = q.Daily ? "daily" : "once";
    const syncMode = () => { repeat.hidden = !q.Daily; once.hidden = q.Daily; };
    mode.addEventListener("change", () => { q.Daily = mode.value === "daily"; syncMode(); return commit(); }); syncMode();
    const startTime = inp("StartAt", { type: "time", "aria-label": "Start time" }); startTime.disabled = !q.StartAtEnabled;
    const stopTime = inp("StopAt", { type: "time", "aria-label": "Stop time" }); stopTime.disabled = !q.StopAtEnabled;
    const post = el("select", { "aria-label": "When the queue finishes" }, ...[["None", "Do nothing"], ["Sleep", "Sleep"], ["Hibernate", "Hibernate"], ["Shutdown", "Shut down"], ["Exit", "Exit FluxDM"], ["Restart", "Restart"]].map(([v, t]) => el("option", { value: v, text: t })));
    post.value = q.PostAction; post.addEventListener("change", () => { q.PostAction = post.value; return commit(); });
    summary = el("p", { class: "sched-summary", text: scheduleSummary(q), "aria-live": "polite" });
    members = el("div", { class: "members", role: "listbox", "aria-label": "Ordered queue items" });
    itemButtons = [
      el("button", { text: "Move to top", onclick: () => moveTo(0) }),
      el("button", { text: "Move up", onclick: () => moveTo(q.Members.findIndex(m => m.id === memberSel) - 1) }),
      el("button", { text: "Move down", onclick: () => moveTo(q.Members.findIndex(m => m.id === memberSel) + 1) }),
      el("button", { text: "Move to bottom", onclick: () => moveTo(q.Members.length - 1) }),
      el("button", { text: "Remove from queue", onclick: () => action(() => invoke("set_items", { ids: [memberSel], patch: { queue: "" } })) }),
      el("button", { text: "Clear queue", onclick: () => action(async () => { if (await confirmBox("Clear queue", `Remove all ${q.Members.length} items from “${q.Name}”? Downloads and files are kept.`, "Clear queue")) await invoke("set_items", { ids: q.Members.map(m => m.id), patch: { queue: "" } }); }) }),
    ];
    right.replaceChildren(
      section("Name", field("Queue name", inp("Name"))),
      section("When", summary, el("div", { class: "row" }, cb("StartAtEnabled", "Start at", () => { startTime.disabled = !q.StartAtEnabled; }), startTime),
        el("div", { class: "row" }, cb("StopAtEnabled", "Stop at", () => { stopTime.disabled = !q.StopAtEnabled; }), stopTime),
        field("Repeat", mode), repeat, once, cb("StartOnLaunch", "Start this queue whenever FluxDM starts")),
      section("Limits", field("Downloads at the same time", inp("MaxConcurrent", { type: "number", min: 1, max: 20 })), field("Speed limit", lim),
        el("p", { class: "hint", text: "Shared by this queue's downloads; 0 = unlimited." }),
        field("Retry failed downloads (0 = off)", inp("RetryCount", { type: "number", min: 0, max: 100 })), field("Delay between retries (seconds)", inp("RetryDelaySec", { type: "number", min: 1, max: 86400 }))),
      section("When the queue finishes", post, cb("ForceShutdown", "Force apps to close (unsaved work in other apps may be lost)"), cb("StopWhenEmpty", "Stop the queue when it is empty"),
        el("p", { class: "hint", text: "You always get a 60-second countdown you can cancel. An empty queue never triggers this." })),
      section("Items", members, el("div", { class: "row sched-actions" }, ...itemButtons)));
    drawMembers(); updateRunButtons();
  }
  const body = el("div", { class: "sched" }, el("div", { class: "sched-sidebar" }, el("b", { text: "Queues" }), list,
    el("div", { class: "row sched-actions" }, el("button", { class: "grow", text: "New queue", onclick: () => action(async () => { cur = await invoke("queue_new"); }) }),
      el("button", { class: "grow", text: "Delete", onclick: () => action(async () => { if (await confirmBox("Delete queue", `Delete the queue “${cur}”? Its downloads stay in the list.`, "Delete", true)) await invoke("queue_delete", { name: cur }); }) }))), right);
  const startBtn = el("button", { text: "Start queue", onclick: () => action(() => invoke("queue_run", { name: cur, on: true })) });
  const stopBtn = el("button", { text: "Stop queue", onclick: () => action(() => invoke("queue_run", { name: cur, on: false })) });
  const timer = setInterval(async () => {
    if (closed || busy || refreshing || saving || dragged) return;
    refreshing = true;
    const requestedRevision = revision;
    try {
      const fresh = await invoke("queues");
      if (closed || busy || saving || dragged || requestedRevision !== revision) return;
      if (fresh.length !== queues.length || fresh.some((q, i) => q.Name !== queues[i]?.Name)) { queues = fresh; if (!selected()) cur = queues[0]?.Name; draw(); }
      else {
        for (const q of queues) { const live = fresh.find(x => x.Name === q.Name); Object.assign(q, { Running: live.Running, Members: live.Members, LastStartSlot: live.LastStartSlot, LastStartFire: live.LastStartFire }); }
        drawList();
        if (selected() && members && JSON.stringify(selected().Members) !== members.dataset.snapshot) drawMembers();
      }
    } catch { /* Refresh failures leave the form usable; explicit actions report errors. */ }
    finally { refreshing = false; if (!closed) updateRunButtons(); }
  }, 1000);
  const m = modal({ title: "Scheduler", body, xwide: true, backdrop: false, onClose: () => { closed = true; clearInterval(timer); }, buttons: [{ label: "Close", primary: true }] });
  m.box.querySelector("footer").prepend(startBtn, stopBtn);
  draw();
}


// ---------------------------------------------------------------- dialogs: stats

async function openStats() {
  const s = await invoke("stats");
  const card = (t, c) => el("div", { class: "card" }, el("div", { class: "t", text: t }), el("b", { text: c.bytes }), el("div", { class: "s", text: c.sub }));
  const canvas = el("canvas", { id: "statChart" });
  const body = el("div", {},
    el("div", { class: "stat-cards" }, card("Today", s.today), card("Last 7 days", s.week), card(s.monthTitle, s.month), card("All time", s.all)),
    canvas,
    el("div", { class: "hint", text: "Downloaded per day — last 30 days" }),
    el("h3", { class: "section", text: "By month" }),
    s.months.length
      ? el("table", { class: "plain" }, el("tr", {}, ...["Month", "Downloaded", "Files", "Time downloading", "Average speed"].map((h) => el("th", { text: h }))),
          ...s.months.map((m) => el("tr", {}, ...[m.month, m.bytes, m.files, m.time, m.avg].map((v) => el("td", { text: v })))))
      : el("div", { class: "hint", text: "Nothing downloaded yet." }));
  modal({
    title: "Statistics", body, wide: true,
    buttons: [
      { label: "Reset statistics", left: true, danger: true, click: async () => { if (await confirmBox("Reset statistics", "Forget all download statistics?", "Reset", true)) { await invoke("reset_stats"); } else return false; } },
      { label: "Close", primary: true },
    ],
  });
  requestAnimationFrame(() => {
    const [g, w, h] = fitCanvas(canvas);
    const max = Math.max(1, ...s.chart.map((d) => d.bytes));
    const bw = w / s.chart.length;
    g.font = "10.5px Inter";
    s.chart.forEach((d, i) => {
      const bh = (d.bytes / max) * (h - 30);
      g.fillStyle = d.bytes ? css("--accent") : css("--bar-bg");
      g.beginPath();
      g.roundRect(i * bw + 2, h - 18 - Math.max(2, bh), bw - 4, Math.max(2, bh), 3);
      g.fill();
      if (i % 5 === 4 || i === s.chart.length - 1) { g.fillStyle = css("--text-3"); g.fillText(d.day, i * bw, h - 4); }
    });
    canvas.title = "";
    canvas.addEventListener("mousemove", (e) => { const i = Math.floor((e.offsetX / w) * s.chart.length); const d = s.chart[i]; canvas.title = d ? `${d.day}: ${d.text}` : ""; });
  });
}

// ---------------------------------------------------------------- dialogs: countdown, firewall, updates

function showCountdown(p) {
  const words = { Exit: "FluxDM will exit", Sleep: "The PC will go to sleep", Hibernate: "The PC will hibernate", Shutdown: "The PC will shut down", Restart: "The PC will restart" };
  const token=String(Date.now())+":"+p.queue;
  let closed=false;
  let unlisten=null;
  const publish=()=>invoke("countdown_toast",{payload:{...p,token,seconds:n,closed}});
  let n = 60;
  const num = el("div", { class: "countdown-num", text: n });
  const m = modal({
    title: `Queue “${p.queue}” finished`,
    body: el("div", {}, el("p", { text: `${words[p.action] || p.action} in` }), num, el("p", { class: "hint", text: "seconds. Cancel to stay as you are." })),
    buttons: [{ label: "Cancel", primary: true }, { label: ({ Exit: "Exit FluxDM now", Sleep: "Sleep now", Hibernate: "Hibernate now", Shutdown: "Shut down now", Restart: "Restart now" })[p.action] || "Run now", value: "now" }],
    onClose: (v) => { if(closed) return; closed=true; clearInterval(t); unlisten?.(); publish(); if (v === "now") invoke("run_post_action", { action: p.action, force: p.force }).catch(e => alertBox("Finish action skipped", String(e))); },
  });
  const t = setInterval(() => {
    n--;
    num.textContent = n; publish();
    if (n <= 0) { clearInterval(t); m.close("now"); }
  }, 1000);
  listen("countdown-choice",choice=>{if(choice.token===token&&!closed) m.close(choice.now?"now":undefined);}).then(off=>{if(closed) off(); else unlisten=off;});
  publish();
}

// Every two weeks while "Start with Windows" is off; it counts as asked only once answered (a tray-only start shows it when opened).
async function askStartWithWindows() {
  if (!(await invoke("startup_prompt_due").catch(() => false))) return;
  const yes = await new Promise((res) => modal({ title: "Start FluxDM with Windows?",
    buttons: [{ label: "Not now", value: false }, { label: "Start with Windows", primary: true, value: true }], onClose: (v) => res(!!v),
    body: el("p", { style: { whiteSpace: "pre-wrap" }, text:
    "FluxDM isn't set to start when you sign in, so scheduled downloads, queues and links from your browser wait until you open it.\n\nStart FluxDM with Windows (quietly, in the tray)? You can change this any time in Settings. If you say not now, FluxDM asks again in two weeks." }) }));
  const on = await invoke("startup_prompt_answer", { enable: !!yes }).catch(() => false);
  if (yes && !on) alertBox("Start with Windows", "Windows didn't accept the startup entry. Try the switch in Settings → General.");
  if (S.settings) S.settings.StartWithWindows = on;
}

async function offerFirewall(p) {
  const remote = p.kind === "remote";
  const ok = remote
    ? await confirmBox("Phone remote", "Windows Firewall will block your phone from reaching FluxDM.\n\nAllow FluxDM on private (home) networks now? Windows will ask for permission.", "Allow phone connections")
    : await confirmBox("Let peers reach you?", "Windows Firewall blocks other BitTorrent peers from connecting to FluxDM, so you only reach the peers you connect out to — usually fewer seeders and slower torrents.\n\nAllow FluxDM on private networks? Windows will ask for permission.", "Allow peer connections");
  if (ok) {
    try { await invoke("firewall_allow", { kind: p.kind }); } catch (e) { alertBox("Firewall", String(e)); }
  }
}

function releaseNotes(text) {
  const body = el("div", { class: "release-notes selectable" });
  let paragraph = [], list;
  const flush = () => { if (paragraph.length) body.append(el("p", { text: paragraph.join(" ") })); paragraph = []; };
  for (const line of String(text).replace(/\r/g, "").split("\n")) {
    const heading = /^#{1,6}\s+(.+)$/.exec(line);
    const bullet = /^\s*(?:[-*+] |\d+\. )(.+)$/.exec(line);
    if (!line.trim() || heading || bullet) flush();
    if (heading) { list = null; body.append(el("h4", { text: heading[1] })); }
    else if (bullet) {
      if (!list) { list = el("ul"); body.append(list); }
      list.append(el("li", { text: bullet[1] }));
    } else { list = null; if (line.trim()) paragraph.push(line.trim()); }
  }
  flush();
  return body;
}

async function showUpdate() {
  const rel = S.update;
  if (!rel) return;
  const prog = el("div", { class: "bar", style: { marginTop: "10px" } }, el("i", { id: "updProg" }));
  modal({
    title: `FluxDM ${rel.version} is available`,
    body: el("div", {}, el("p", { text: `You have ${(await invoke("init")).version}. The update downloads ${rel.size ? fmtSize(rel.size) : "the new FluxDM.exe"}, checks its SHA-256 and restarts FluxDM. Running downloads pause and continue after.` }),
      rel.notes ? releaseNotes(rel.notes) : null, prog),
    buttons: [{ label: "Later" }, {
      label: "Update now", primary: true,
      click: async (btn) => {
        btn.disabled = true;
        btn.textContent = "Downloading…";
        try { await invoke("install_update", { release: rel }); } catch (e) { btn.disabled = false; btn.textContent = "Update now"; alertBox("Update", String(e)); }
        return false;
      },
    }],
  });
}

// ---------------------------------------------------------------- drag & drop

function wireDrop() {
  let depth = 0;
  const hint = $("#dropHint");
  const isExternal = (e) => e.dataTransfer && !e.dataTransfer.types.includes("text/fluxcol") && (e.dataTransfer.types.includes("Files") || e.dataTransfer.types.includes("text/plain") || e.dataTransfer.types.includes("text/uri-list"));
  addEventListener("dragenter", (e) => { if (isExternal(e)) { depth++; hint.hidden = false; } });
  addEventListener("dragleave", (e) => { if (isExternal(e) && --depth <= 0) { depth = 0; hint.hidden = true; } });
  addEventListener("dragover", (e) => { if (isExternal(e)) { e.preventDefault(); e.dataTransfer.dropEffect = "copy"; } });
  addEventListener("drop", async (e) => {
    if (!isExternal(e)) return;
    e.preventDefault();
    depth = 0;
    hint.hidden = true;
    const files = [...e.dataTransfer.files];
    if (files.length) {
      const texts = [];
      for (const f of files) {
        const n = f.name.toLowerCase();
        if (n.endsWith(".torrent")) {
          try { await invoke("dropped_torrent", { bytes: [...new Uint8Array(await f.arrayBuffer())] }); } catch (err) { alertBox("Torrent", `“${f.name}”: ${err}`); }
        } else if (n.endsWith(".txt")) texts.push(await f.text());
      }
      const t = texts.join("\n").trim();
      if (t) invoke("open_add", { urls: t });
      return;
    }
    const t = (e.dataTransfer.getData("text/uri-list") || e.dataTransfer.getData("text/plain") || "").split("\n").filter((l) => !l.startsWith("#")).join("\n").trim();
    if (t) invoke("open_add", { urls: t });
  });
}

// ---------------------------------------------------------------- wiring

function navQueue() { return S.nav.startsWith("q:") ? S.nav.slice(2) : null; }

function focusSearch() {
  $("#search").classList.add("open");
  $("#searchToggle").setAttribute("aria-expanded", "true");
  $("#searchBox").focus();
}

function responsiveToolbar() {
  const more = $("#btnMore");
  more.hidden = false;
  more.onclick = () => {
    const r = more.getBoundingClientRect();
    showMenu([
      { label: "Pause all", click: act.pauseAll }, { label: "Clear finished", click: act.clearFinished },
      { label: "Columns...", click: () => columnsMenu(r.left, r.bottom) },
      { label: "Table view", checked: !!S.tableView, click: () => { S.tableView = !S.tableView; document.body.classList.toggle("table-view", S.tableView); buildHead(); rebuildAllRows(); saveLayout(); } },
    ], r.left, r.bottom + 4);
  };
  $("#searchToggle").onclick = focusSearch;
  const closeSearch = () => {
    $("#search").classList.remove("open");
    $("#searchToggle").setAttribute("aria-expanded", "false");
  };
  $("#search").addEventListener("focusout", (e) => { if (!$("#search").contains(e.relatedTarget)) closeSearch(); });
  $("#searchBox").addEventListener("keydown", (e) => {
    if (e.key === "Escape") { e.stopPropagation(); closeSearch(); $("#searchToggle").focus(); }
  });
}

function wire() {
  $("#btnAdd").onclick = () => invoke("open_add", { queue: navQueue() });
  $("#btnResume").onclick = act.resume;
  $("#btnPause").onclick = act.pause;
  $("#btnPauseAll").onclick = act.pauseAll;
  $("#btnDelete").onclick = act.delete;
  $("#btnClear").onclick = act.clearFinished;
  $("#btnScheduler").onclick = () => openScheduler();
  $("#btnStats").onclick = openStats;
  $("#btnColumns").onclick = (e) => { const r = (e.currentTarget.hidden ? $("#btnMore") : e.currentTarget).getBoundingClientRect(); columnsMenu(r.left, r.bottom + 4); };
  $("#btnSettings").onclick = openSettings;
  $("#btnUpdate").onclick = showUpdate;
  responsiveToolbar();
  const groupSelect = $("#groupSelect");
  GROUP_OPTIONS.forEach(key => groupSelect.append(el("option", { value: key, text: key })));
  groupSelect.onchange = () => { S.groupBy = groupSelect.value; arrange(); saveLayout(); };
  const sortSelect = $("#sortSelect");
  for (const [key, label] of [["Added", "Date added"], ["File name", "Name"], ["Size", "Size"], ["Progress", "Progress"], ["Speed", "Download speed"], ["Up Speed", "Upload speed"], ["Time left", "Time left"], ["Status", "Status"], ["Category", "Type"]]) sortSelect.append(el("option", { value: key, text: label }));
  const showSort = updateSortControls;
  sortSelect.onchange = () => { S.sortKey = sortSelect.value; buildHead(); arrange(); saveLayout(); };
  $("#sortDirection").onclick = () => { S.sortDir *= -1; buildHead(); arrange(); saveLayout(); };
  showSort();
  $("#btnQueues").onclick = () => { const r = $("#btnQueues").getBoundingClientRect(); showMenu([
    { label: "Queues and scheduler...", click: () => openScheduler() },
    { label: "Unfinished", click: () => { S.nav = "unfinished"; renderNav(); arrange(); } },
    ...[...new Set([...S.rows.values()].flatMap((r) => r.tags))].map((tag) => ({ label: "Tag: " + tag, click: () => { S.nav = "tag:" + tag; renderNav(); arrange(); } })),
    ...(S.summary?.queues || []).map((q) => ({ label: q.name + (q.running ? " · running" : ""), click: () => { S.nav = "q:" + q.name; renderNav(); arrange(); } })),
  ], r.right, r.top); };
  $("#queueStart").onclick = () => invoke("queue_run", { name: S.nav.slice(2), on: true });
  $("#queueStop").onclick = () => invoke("queue_run", { name: S.nav.slice(2), on: false });

  $("#searchBox").addEventListener("input", (e) => { S.search = e.target.value; arrange(); });

  const rows = $("#rows");
  rows.addEventListener("mousedown", (e) => {
    const tr = e.target.closest("tr.r");
    if (!tr) return;
    if (e.button === 2 && S.sel.has(tr.dataset.id)) { S.cur = tr.dataset.id; paintSel(); renderDetails(); return; }
    onRowClick(e, tr.dataset.id);
  });
  rows.addEventListener("dblclick", (e) => {
    const tr = e.target.closest("tr.r");
    if (!tr) return;
    const r = S.rows.get(tr.dataset.id);
    if (r.status === "Completed") invoke("open_item", { id: r.id });
    else invoke("toggle", { ids: [r.id] });
  });
  rows.addEventListener("contextmenu", (e) => { e.preventDefault(); if (e.target.closest("tr.r")) rowMenu(e.clientX, e.clientY); });
  $("#listWrap").addEventListener("mousedown", (e) => { if (e.target.id === "listWrap" || e.target.id === "list" || e.target.closest("#emptyList")) selectOnly(null); });

  $$(".tab").forEach((b) => (b.onclick = () => setTab(b.dataset.tab)));
  $("#dCopy").onclick = () => { const r = S.rows.get(S.cur); if (r?.torrent) { selectOnly(r.id); act.copyTorrent("magnet"); } else if (r) invoke("copy_text", { text: r.url }); };
  const single = (action) => () => { const id = S.cur; if (id) { S.sel = new Set([id]); paintSel(); action(); } };
  $("#dToggle").onclick = single(act.toggle);
  $("#dFolder").onclick = single(act.folder);
  $("#dOpen").onclick = single(act.open);
  $("#dRefresh").onclick = single(act.refreshLink);
  $("#dAddMirror").onclick = single(act.addMirror);
  $("#mirrorAdd").onclick = single(act.addMirror);
  $("#dMore").onclick = single(() => { const b = $("#dMore").getBoundingClientRect(); rowMenu(b.left,b.bottom); });
  $("#dConn").addEventListener("input", (e) => ($("#dConnVal").textContent = e.target.value));
  $("#dConn").addEventListener("change", (e) => invoke("set_items", { ids: [S.cur], patch: { connections: +e.target.value } }));
  detailLimit = speedCombo($("#dLimitBox"), {
    get: () => S.rows.get(S.cur)?.limitKBps || 0,
    set: (k) => { const r = S.rows.get(S.cur); if (r) { r.limitKBps = k; invoke("set_items", { ids: [r.id], patch: { limitKBps: k } }); } },
  });
  const phaseLimit = (box, key) => speedCombo($(box), {
    presets: UPLOAD_PRESETS, allowNone: true,
    get: () => S.rows.get(S.cur)?.[key] ?? 0,
    set: k => { const r = S.rows.get(S.cur); if (r?.torrent) { r[key] = k; invoke("set_items", { ids: [r.id], patch: { [key]: k } }); } },
  });
  detailUpload = phaseLimit("#dUploadBox", "uploadLimitKBps");
  detailSeeding = phaseLimit("#dSeedingBox", "seedingLimitKBps");
  $("#dOpenDone").addEventListener("change", (e) => { const r = S.rows.get(S.cur); if (r) { r.openWhenDone = e.target.checked; invoke("set_items", { ids: [r.id], patch: { openWhenDone: e.target.checked } }); } });
  const commitHash = () => { const r = S.rows.get(S.cur); if (r && $("#dHash").value.trim() !== r.hash) invoke("set_items", { ids: [r.id], patch: { hash: $("#dHash").value.trim() } }); };
  $("#dHash").addEventListener("keydown", (e) => { if (e.key === "Enter") e.target.blur(); });
  $("#dHash").addEventListener("blur", commitHash);
  $("#fileTable").addEventListener("click", (e) => {
    const b = e.target.closest("[data-toggle]");
    if (!b) return;
    const closed = fileTreeClosed(S.cur), path = b.dataset.toggle;
    closed.has(path) ? closed.delete(path) : closed.add(path);
    refreshDetails();
  });
  $("#fileTable").addEventListener("change", async (e) => {
    const folder = e.target.dataset.folder, folderPrio = e.target.dataset.folderPrio;
    if (folder != null || folderPrio != null) {
      const id = S.cur, indices = S.fileFolders?.get(folder ?? folderPrio) || [];
      e.target.disabled = true;
      try {
        for (const index of indices) {
          if (folder != null) await invoke("torrent_file", { id, index, wanted: e.target.checked });
          else if (+e.target.value >= 0) await invoke("torrent_file_priority", { id, index, level: +e.target.value });
        }
      } finally { e.target.disabled = false; refreshDetails(); }
      return;
    }
    const i = e.target.dataset.file;
    if (i != null) invoke("torrent_file", { id: S.cur, index: +i, wanted: e.target.checked }).then(refreshDetails);
    const p = e.target.dataset.prio;
    if (p != null) {
      e.target.blur();
      invoke("torrent_file_priority", { id: S.cur, index: +p, level: +e.target.value }).then(refreshDetails);
    }
  });

  uploadCombo = speedCombo($("#uploadBox"), { presets: UPLOAD_PRESETS, allowNone: true,
    get: () => S.settings?.TorrentUploadLimitKBps ?? 0,
    set: async (k) => { await invoke("set_global_upload_limit", { kbps: k }); S.settings.TorrentUploadLimitKBps = k; S.settings.TorrentSeedingLimitKBps = k; showLimit(); },
  });
  // global limit combo
  globalCombo = speedCombo($("#limitBox"), { get: () => S.settings?.GlobalSpeedLimitKBps || 0, set: setGlobalLimit });

  splitter($("#railResize"), e => setRailWidth(e.clientX - $("#sidebar").getBoundingClientRect().left), () => { if (S.railWidth < 150) setRailWidth(62); });
  $("#railResize").ondblclick = () => { setRailWidth(S.railWidth >= 150 ? 62 : 180); saveLayout(); };
  $("#railResize").onkeydown = e => {
    if (["ArrowLeft", "ArrowRight", "Enter"].includes(e.key)) {
      e.preventDefault(); setRailWidth(e.key === "Enter" ? S.railWidth >= 150 ? 62 : 180 : e.key === "ArrowRight" ? S.railWidth < 150 ? 150 : S.railWidth + 10 : S.railWidth <= 150 ? 62 : S.railWidth - 10); saveLayout();
    }
  };
  addEventListener("resize", () => setRailWidth(S.railWidth || 62));
  splitter($("#splitSidebar"), (e) => {
    const r = $("#content").getBoundingClientRect();
    $("#downloadColumn").style.width = Math.max(260, Math.min(600, r.width - 240, e.clientX - r.left)) + "px";
    drawSeg(); drawGraph();
  });
  splitter($("#splitGen"), (e) => {
    const r = $("#tab-general").getBoundingClientRect();
    const f = Math.max(0.05, Math.min(0.95, (e.clientX - r.left) / r.width));
    $("#genLeft").style.flex = `${f} 1 0`;
    $("#genRight").style.flex = `${1 - f} 1 0`;
    drawSeg(); drawGraph();
  });
  addEventListener("resize", () => { drawSeg(); drawGraph(); });
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => { drawSeg(); drawGraph(); });

  wireDrop();

  // A row picture that fails to load falls back to the category glyph.
  document.addEventListener("load", e => {
    const img = e.target;
    if (img.tagName === "IMG" && img.parentElement?.classList.contains("row-art")) sampleCardArt(img.parentElement, img.src);
  }, true);
  document.addEventListener("error", (e) => {
    const img = e.target;
    if (img.id === "dCover") { img.dataset.failed = img.getAttribute("src"); img.hidden = true; $("#detailGlyph").hidden = false; }
    if (img.tagName === "IMG" && img.parentElement?.id === "bannerArt") {
      const r = S.rows.get(S.cur);
      const fallback = r?.icon || r?.cover || fallbackArt(r?.torrent ? "magnet" : CATEGORY_ICON[r?.category] || "file");
      img.className = "icon-wash";
      img.src = img.getAttribute("src") === fallback ? fallbackArt("file") : fallback;
    }
    if (img.tagName === "IMG" && img.dataset.cat) img.parentElement.innerHTML = icon(CATEGORY_ICON[img.dataset.cat] || "file");
  }, true);

  document.addEventListener("keydown", async (e) => {
    if ($(".modal-back")) return;
    const inText = e.target.matches("input, textarea, select");
    if (e.ctrlKey && e.key.toLowerCase() === "n") { e.preventDefault(); invoke("open_add", { queue: navQueue() }); return; }
    if (e.ctrlKey && e.key.toLowerCase() === "f") { e.preventDefault(); focusSearch(); $("#searchBox").select(); return; }
    if (inText) return;
    if (e.ctrlKey && e.key.toLowerCase() === "v") { e.preventDefault(); const u = await invoke("clipboard_url"); invoke("open_add", { urls: u || "" }); return; }
    if (e.ctrlKey && e.key.toLowerCase() === "a") { e.preventDefault(); S.sel = new Set(visibleIds()); paintSel(); return; }
    if (e.key === "Delete") { e.preventDefault(); act.delete(); }
    else if (e.key === "Enter") { e.preventDefault(); act.open(); }
    else if (e.key === " ") { e.preventDefault(); if (S.sel.size) act.toggle(); }
    else if (e.key === "ArrowDown") { e.preventDefault(); moveCursor(1, e.shiftKey); }
    else if (e.key === "ArrowUp") { e.preventDefault(); moveCursor(-1, e.shiftKey); }
    else if (e.key === "Home") { e.preventDefault(); moveCursor(-1e9, e.shiftKey); }
    else if (e.key === "End") { e.preventDefault(); moveCursor(1e9, e.shiftKey); }
    else if (e.key === "Escape") selectOnly(null);
  });
}
// Calendar groups use local midnight so daylight-saving days keep their labels.
function dayLabel(timestamp, now = new Date()) {
  if (!Number.isFinite(timestamp) || timestamp <= 0) return "UNKNOWN DATE";
  const date = new Date(timestamp * 1000);
  const key = (d) => `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
  if (key(date) === key(now)) return "TODAY";
  const yesterday = new Date(now); yesterday.setDate(yesterday.getDate() - 1);
  if (key(date) === key(yesterday)) return "YESTERDAY";
  return date.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" }).toUpperCase();
}

/** Whether a card shows its art layer. Kept out of cardHtml so selecting a row never rebuilds (and blinks) it. */
const cardShowsArt = (r) => S.settings?.ArtSelectedOnly === false || S.cur === r.id;

function cardHtml(r) {
  const type = (r.game ?? !!r.banner) ? "game" : r.torrent ? "torrent" : /AI/i.test(r.category) ? "ai" : /Video/i.test(r.category) ? "video" : /Program/i.test(r.category) ? "program" : /Compress|Archive/i.test(r.category) ? "archive" : "file";
  const glyph = (r.game ?? !!(r.banner || r.cover)) ? "game" : CATEGORY_ICON[r.category] || (r.torrent ? "magnet" : "file");
  const picture = r.icon || r.cover;
  const status = statusLabel(r);
  const progress = Math.max(0, Math.min(100, Number(r.progress) || 0));
  const downloaded = fmtSize(r.downloaded), total = r.sizeText || "?";
  const size = r.status === "Completed" ? total : (downloaded.split(" ").at(-1) === total.split(" ").at(-1) ? downloaded.split(" ")[0] : downloaded) + "/" + total;
  const extras = visibleCols().filter((c) => !["#", "File name", "Size", "Progress", "Status", "Speed", "Time left", "Up Speed", "Conn."].includes(c.k));
  return `<div class="download-card type-${type}">
    <div class="row-art" data-art-color="${esc(r.artColor || "")}"><img src="${esc(r.banner || picture || fallbackArt(glyph))}" class="${r.banner ? "" : "icon-wash"}" alt="" decoding="async"></div>
    <span class="card-icon">${picture ? `<img src="${esc(picture)}" alt="" data-cat="${esc(r.category)}">` : icon(glyph)}</span>
    <div class="card-body"><div class="card-heading"><strong title="${esc(r.name)}">${esc(r.title || r.name)}</strong><span class="pill st-${r.seeding ? "Seeding" : esc(r.status)}" title="${esc(r.statusText)}">${esc(status)}</span></div>
    ${r.title && r.title !== r.name ? `<small class="card-filename" title="${esc(r.name)}">${esc(r.name)}</small>` : ""}
    <div class="card-progress"><i style="width:${progress}%"></i></div>
    <div class="card-meta"><span title="${esc(r.downloadedText || size)}">${esc(size)}</span>${rowActivity(r) ? `<span class="activity" title="${esc(rowActivity(r))}">${esc(rowActivity(r))}</span>` : `<span class="down">↓ ${esc(r.speedText || "0 B/s")}</span><span>${esc(r.eta || "—")}</span><span class="${r.torrent ? "up" : ""}">${r.torrent ? "↑ " + esc(r.upSpeedText || "0 B/s") : esc(r.conn || "—")}</span>`}</div>
    ${extras.length ? `<div class="card-extra">${extras.map((c) => `<span title="${esc(c.k)}">${esc(c.k)}: ${cellHtml(c.k, r)}</span>`).join("")}</div>` : ""}</div></div>`;
}

/** The selected card's art: its average brightness (0..1) once the card's own dimming is applied, and whether
 *  it's a game banner (drawn directly) or an icon/cover (blurred under a dark overlay). */
let cardArt = { level: 0, banner: false };
/** Text on the selected card follows the brightness the art is really shown at: dark text with a soft light
 *  halo on bright art, light text with a shadow on medium art, unchanged on faint art. */
function updateCardText(strength = (Number(S.settings?.CardArtStrength) || 100) / 100) {
  // Banners: brightness(level * .45 * strength). Icons/covers: level, under an overlay of ~82% whose opacity
  // is min(1, 1 / strength) (see the .row-art rules in main.css).
  const shown = cardArt.banner ? cardArt.level * .45 * strength : cardArt.level * (1 - .82 * Math.min(1, 1 / strength));
  document.documentElement.dataset.cardText = shown >= .34 ? "dark" : shown >= .16 ? "shadow" : "plain";
}let artKey = "", artGeneration = 0, bannerSwap = 0;
const accentCache = new Map();
/** Treatments already worked out, so switching back to a download applies its colours in the same frame. */
const accentDone = new Map();
function updateWindowArt() {
  const r = S.rows.get(S.cur), st = S.settings || {};
  const banner = r?.banner || r?.icon || r?.cover || "";
  const key = JSON.stringify([banner, st.ArtBehindWindow, st.AccentFromArt, st.AccentColor]);
  if (key === artKey) return;
  artKey = key; const generation = ++artGeneration;
  const root = document.documentElement;
  const fallback = /^#[0-9a-f]{6}$/i.test(st.AccentColor || "") ? st.AccentColor : "#e9c98f";
  const apply = treatment => {
    root.style.setProperty("--art-brightness", String(treatment?.brightness ?? (banner ? ART_FALLBACK_BRIGHTNESS : .035)));
    root.style.setProperty("--art-card-brightness", String(treatment?.cardBrightness ?? .5));
    root.style.setProperty("--art-adaptive-blur", (treatment?.blur ?? 48) + "px");
    root.style.setProperty("--accent", st.AccentFromArt !== false ? treatment?.accent || fallback : fallback);
    // The picture's average brightness after the card's own brightness(min(.85, 110 / luminance)).
    const lum = treatment?.luminance || 0;
    cardArt = { level: lum ? Math.min(.85 * lum, 110) / 255 : 0, banner: !!r?.banner };
    updateCardText();
  };
  const setArt = () => { $("#windowArt").style.backgroundImage = banner && st.ArtBehindWindow !== false ? `url(${JSON.stringify(banner)})` : "none"; };
  // Never reset to the defaults first and then switch to the art's colours a frame later: that was a flash
  // of the whole window on every click. Known art changes at once; new art keeps the current look until its
  // colours are ready, then the picture and the colours change together.
  if (!banner) { setArt(); apply(null); return; }
  if (accentDone.has(banner)) { setArt(); apply(accentDone.get(banner)); return; }
  if (!accentCache.has(banner)) {
    accentCache.set(banner, new Promise(resolve => {
      const img = new Image();
      img.crossOrigin = "anonymous";
      img.onload = () => {
        try {
          const canvas = document.createElement("canvas"); canvas.width = 48; canvas.height = 24;
          const ctx = canvas.getContext("2d", { willReadFrequently: true }); ctx.drawImage(img, 0, 0, 48, 24);
          resolve(artTreatment(ctx.getImageData(0, 0, 48, 24).data));
        } catch { resolve(null); }
      };
      img.onerror = () => resolve(null);
      img.src = banner;
    }));
  }
  Promise.all([accentCache.get(banner), preloadArt(banner)]).then(([treatment]) => {
    accentDone.set(banner, treatment);
    if (generation === artGeneration) { setArt(); apply(treatment); }
  });
}
async function refreshFreeSpace() {
  const folder = S.settings?.DefaultFolder;
  if (!folder) return;
  try { const bytes = await invoke("free_space", { folder }); $("#statusFree").textContent = bytes == null ? "Space unavailable" : `${fmtSize(bytes)} free`; $("#statusFree").title = folder; } catch { $("#statusFree").textContent = "Space unavailable"; }
}
