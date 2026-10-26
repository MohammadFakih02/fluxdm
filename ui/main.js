// FluxDM main window: the download list, sidebar, details pane, and the Settings / Scheduler / Stats dialogs.
"use strict";

const COLUMNS = [
  { k: "#", w: 42, help: "The row number", num: true },
  { k: "File name", w: 320, sort: (r) => r.name.toLowerCase() },
  { k: "Size", w: 90, help: "How big the download is", sort: (r) => r.total, num: true },
  { k: "Progress", w: 150, help: "How much of it is downloaded so far", sort: (r) => r.progress },
  { k: "Status", w: 170, help: "Downloading, paused, finished, or what went wrong", sort: (r) => r.status },
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
  sortKey: null,
  sortDir: 1,
  cols: [], // [{k, w, visible}] in display order
  tab: "general",
  details: null,
  update: null,
};

const trs = new Map(); // id -> <tr>

// ---------------------------------------------------------------- start-up

(async function init() {
  $$("[data-icon]").forEach((e) => (e.innerHTML = icon(e.dataset.icon)));
  const d = await invoke("init");
  S.settings = d.settings;
  S.categories = d.categories;
  applyTheme(S.settings.Theme);
  loadLayout();
  buildHead();
  wire();
  await listen("list", onList);
  await listen("settings-changed", async () => {
    const g = await invoke("get_settings");
    S.settings = g.settings;
    applyTheme(S.settings.Theme);
    showLimit();
  });
  await listen("countdown", showCountdown);
  await listen("offer-firewall", offerFirewall);
  await listen("update-available", (rel) => { S.update = rel; $("#updateBadge").hidden = false; $("#btnUpdate").textContent = `Update to ${rel.version}`; });
  await listen("update-progress", (p) => { const b = $("#updProg"); if (b) b.style.width = p + "%"; });
  showLimit();
  await invoke("ready");
  setInterval(refreshDetails, 1000);
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
  if (S.cur && p.changed.some((r) => r.id === S.cur)) renderDetails();
  if (!S.cur && !$("#detailsPane").hidden) renderDetails();
}

function visibleCols() { return S.cols.filter((c) => c.visible); }
function colDef(k) { return COLUMNS.find((c) => c.k === k); }

function statusClass(r) { return "st-" + r.status; }

function cellHtml(k, r) {
  switch (k) {
    case "#": return "";
    case "File name": {
      const ic = r.icon ? `<img src="${esc(r.icon)}" data-cat="${esc(r.category)}">` : icon(CATEGORY_ICON[r.category] || "file");
      return `<div class="cell-name" title="${esc(r.path)}"><span class="ic">${ic}</span><span class="nm">${esc(r.name)}</span></div>`;
    }
    case "Size": return esc(r.sizeText);
    case "Progress": {
      const cls = r.status === "Completed" ? "done" : r.status === "Error" ? "err" : r.running || r.status === "Queued" ? "" : "paused";
      return `<div class="cell-prog"><span class="pt">${esc(r.progressText)}</span><div class="bar ${cls}"><i style="width:${r.progress}%"></i></div></div>`;
    }
    case "Status": return `<span class="${statusClass(r)}" title="${esc(r.statusText)}">${esc(r.statusText)}</span>`;
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
  if (nav === "active") return ["Connecting", "Downloading", "Verifying"].includes(r.status);
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

function sortedIds() {
  let ids = S.order.filter((id) => S.rows.has(id));
  if (S.sortKey) {
    const def = colDef(S.sortKey);
    const f = def.sort || ((r) => cellHtml(S.sortKey, r));
    const idx = new Map(ids.map((id, i) => [id, i]));
    ids.sort((a, b) => {
      const x = f(S.rows.get(a)), y = f(S.rows.get(b));
      return (x < y ? -1 : x > y ? 1 : idx.get(a) - idx.get(b)) * S.sortDir;
    });
  }
  return ids;
}

// Puts rows in order, hides filtered ones and renumbers "#".
function arrange() {
  const tbody = $("#rows");
  const ids = sortedIds();
  let n = 0, prev = null;
  const numIdx = visibleCols().findIndex((c) => c.k === "#");
  for (const id of ids) {
    const tr = trs.get(id);
    if (!tr) continue;
    const show = matches(S.rows.get(id));
    tr.classList.toggle("hide", !show);
    if (show) {
      n++;
      if (numIdx >= 0 && tr.children[numIdx].textContent !== String(n)) tr.children[numIdx].textContent = n;
    }
    const want = prev ? prev.nextSibling : tbody.firstChild;
    if (want !== tr) tbody.insertBefore(tr, want);
    prev = tr;
  }
  $("#emptyList").hidden = n > 0;
  for (const id of [...S.sel]) if (!S.rows.has(id)) S.sel.delete(id);
}

function visibleIds() { return sortedIds().filter((id) => matches(S.rows.get(id))); }

// ---------------------------------------------------------------- columns & layout

function loadLayout() {
  const st = S.settings;
  const saved = st.Columns || {};
  S.cols = COLUMNS.map((c, i) => {
    const s = saved["Downloads:" + c.k];
    return { k: c.k, w: s && s.Width > 0 ? s.Width : c.w, visible: s ? s.Visible : SIMPLE.includes(c.k), order: s ? s.Order : i };
  });
  S.cols.sort((a, b) => a.order - b.order);
  if ((st.ColumnsLayoutVersion || 0) < 2) S.cols.forEach((c) => (c.visible = SIMPLE.includes(c.k)));
  S.cols.find((c) => c.k === "File name").visible = true;
  if (st.SidebarWidth > 40) $("#sidebar").style.width = st.SidebarWidth + "px";
  if (st.DetailsHeight > 0) $("#details").style.height = st.DetailsHeight + "px";
  const split = st.DetailsSplit;
  if (split > 0 && split < 1) { $("#genLeft").style.flex = `${split} 1 0`; $("#genRight").style.flex = `${1 - split} 1 0`; }
  for (const k of ["Trackers", "Peers", "Content"]) S["w" + k] = {};
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
    for (const g of ["Trackers", "Peers", "Content"]) for (const [n, w] of Object.entries(S["w" + g] || {})) columns[`${g}:${n}`] = { Width: w, Order: 0, Visible: true };
    const l = $("#genLeft").getBoundingClientRect().width, r = $("#genRight").getBoundingClientRect().width;
    invoke("save_layout", {
      columns,
      sidebar: $("#sidebar").getBoundingClientRect().width,
      details: $("#details").getBoundingClientRect().height,
      split: l + r > 0 ? l / (l + r) : 0.5,
    });
  }, 400);
}

function buildHead() {
  const head = $("#head");
  head.replaceChildren();
  for (const c of visibleCols()) {
    const th = el("th", { title: colDef(c.k).help || null, draggable: "true", style: { width: c.w + "px" } });
    th.dataset.k = c.k;
    th.append(c.k === "#" ? "#" : c.k);
    if (S.sortKey === c.k) th.append(el("span", { class: "sort", text: S.sortDir > 0 ? "▲" : "▼" }));
    const grip = el("span", { class: "grip" });
    grip.addEventListener("mousedown", (e) => startColResize(e, c, th));
    grip.addEventListener("click", (e) => e.stopPropagation());
    th.append(grip);
    th.addEventListener("click", () => {
      if (c.k === "#") { S.sortKey = null; }
      else if (S.sortKey === c.k) { if (S.sortDir < 0) S.sortKey = null; else S.sortDir = -1; }
      else { S.sortKey = c.k; S.sortDir = 1; }
      buildHead();
      arrange();
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
  $("#list").style.minWidth = visibleCols().reduce((a, c) => a + c.w, 0) + "px";
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
  const up = () => { removeEventListener("mousemove", move); removeEventListener("mouseup", up); saveLayout(); };
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
function splitter(handle, onMove) {
  handle.addEventListener("mousedown", (e) => {
    e.preventDefault();
    handle.classList.add("drag");
    const move = (ev) => onMove(ev);
    const up = () => { handle.classList.remove("drag"); removeEventListener("mousemove", move); removeEventListener("mouseup", up); saveLayout(); };
    addEventListener("mousemove", move);
    addEventListener("mouseup", up);
  });
}

// ---------------------------------------------------------------- sidebar

function renderNav() {
  const rows = [...S.rows.values()];
  const count = (key) => rows.filter((r) => matchesNav(r, key)).length;
  const items = [
    ["all", "All downloads", "all"], ["active", "Active", "down"], ["unfinished", "Unfinished", "pause"], ["finished", "Finished", "check"],
    { head: "CATEGORIES" },
    ...S.categories.map((c) => ["cat:" + c, c, CATEGORY_ICON[c] || "file"]),
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
      const row = el("div", { class: "nav-item" + (k === S.nav ? " active" : ""), html: icon(ic) });
      row.dataset.k = k;
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
  if (s.schedule) t += ` · speed schedule active (${s.effectiveLimitText})`;
  $("#statusLeft").textContent = t;
  $("#statusTorrent").textContent = s.torrent || "";
  if (document.activeElement !== $("#limitInput") && S.settings && s.limitKBps !== S.settings.GlobalSpeedLimitKBps) {
    S.settings.GlobalSpeedLimitKBps = s.limitKBps;
    showLimit();
  }
}

let globalCombo = null, detailLimit = { show() {} };
function showLimit() {
  if (globalCombo) globalCombo.show();
}

async function setGlobalLimit(k) {
  S.settings.GlobalSpeedLimitKBps = k;
  await invoke("set_global_limit", { kbps: k });
  showLimit();
}

// ---------------------------------------------------------------- selection

function selectOnly(id) { S.sel = new Set(id ? [id] : []); S.cur = id; S.anchor = id; paintSel(); renderDetails(); }
function paintSel() {
  for (const [id, tr] of trs) { tr.classList.toggle("sel", S.sel.has(id)); tr.classList.toggle("cur", S.cur === id); }
}
function selected() { return [...S.sel].filter((id) => S.rows.has(id)).map((id) => S.rows.get(id)); }
function selIds() { return selected().map((r) => r.id); }

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
      const v = await new Promise((res) => modal({
        title: "Delete",
        body: el("p", { text: `Remove ${what} from the list?` }),
        buttons: [
          { label: "Cancel", value: null, left: true },
          { label: "Keep files, just remove from list", value: "keep" },
          { label: "Also move the files to the Recycle Bin", value: "trash", danger: true },
        ],
        onClose: res,
      }));
      if (!v) return;
      await invoke("remove", { ids: sel.map((r) => r.id), deleteFiles: v === "trash" });
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
    const url = await promptBox("Refresh link", "Paste a fresh link for this file (e.g. copied again from the website). If the size matches, the bytes you already have are kept.", r.url, { mono: true });
    if (!url || url.trim() === r.url) return;
    if (!/^https?:\/\//i.test(url.trim())) return alertBox("Refresh link", "That isn't an http(s) link.");
    let p;
    try { p = await invoke("probe", { url: url.trim(), headers: "" }); }
    catch (e) { return alertBox("Refresh link", "Couldn't reach the new link: " + e); }
    let restart = false;
    if (r.total > 0 && p.total !== r.total) {
      if (!(await confirmBox("Refresh link", `The new link is ${fmtSize(p.total)} but the original was ${fmtSize(r.total)} — it's a different file.\nUse it anyway and start from zero?`, "Use it", true))) return;
      restart = true;
    }
    await invoke("refresh_link", { id: r.id, url: url.trim(), restart });
  },
  async addMirror() {
    const r = S.rows.get(S.cur);
    if (!r) return;
    if (r.video || r.torrent) return alertBox("Backup link", "Backup links are for normal file downloads, not video pages or torrents.");
    const text = await promptBox("Add backup link", "Another link for exactly the same file (a mirror). FluxDM spreads connections across all links, and keeps going from the others if one dies. Several links can be pasted, one per line.", "", { multiline: true, mono: true });
    if (!text || !text.trim()) return;
    try { await invoke("add_mirrors", { id: r.id, text }); } catch (e) { alertBox("Backup link", String(e)); }
  },
  async editTags() {
    const sel = selected();
    if (!sel.length) return;
    let common = sel[0].tags;
    for (const r of sel.slice(1)) common = common.filter((t) => r.tags.some((x) => x.toLowerCase() === t.toLowerCase()));
    const text = await promptBox("Edit tags", "Comma-separated labels for organising downloads (e.g. Games, Movies) — separate from the automatic file-type category. Leave empty to clear.", common.join(", "));
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
      const verdict = r.hash.length === 64 ? (h.toLowerCase() === r.hash.toLowerCase() ? "\n\n✓ Matches the expected checksum." : "\n\n✗ Does NOT match the expected checksum!") : "";
      alertBox(`SHA-256 — ${r.name}`, `${h}\n\n(copied to clipboard)${verdict}`);
    } catch (e) { alertBox("SHA-256", String(e)); }
  },
  moveTo: (q) => invoke("set_items", { ids: selIds(), patch: { queue: q } }),
  async recheck() { try { await invoke("torrent_recheck", { ids: selIds() }); } catch (e) { alertBox("Force recheck", String(e)); } },
  async reannounce() {
    const n = await invoke("torrent_reannounce", { ids: selIds() });
    if (!n) alertBox("Force reannounce", "Only running torrents can reannounce — resume it first.");
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
    const text = await promptBox("Add trackers", "Tracker URLs to add (one per line or separated by spaces), e.g. udp://tracker.opentrackr.org:1337/announce. More trackers can find more peers; they're kept for this torrent from now on.", "", { multiline: true, mono: true });
    if (!text || !text.trim()) return;
    try { await invoke("torrent_add_trackers", { ids, text }); } catch (e) { alertBox("Add trackers", String(e)); }
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
    const name = await promptBox("Find cover", "Game name to search for (Steam's store is searched):", guess);
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
    { label: torrents && sel.every((r) => r.status === "Completed") ? "Resume sharing" : "Resume", click: act.resume },
    { label: torrents && sel.every((r) => r.status === "Completed") ? "Stop sharing" : "Pause", click: act.pause },
    { label: "Restart from zero", click: act.restart },
    !torrents && { label: "Refresh link…", click: act.refreshLink },
    { label: torrents ? "Limit download speed" : "Limit speed", sub: await limitMenuItems("Limit speed", "Download speed limit", "limitKBps", LIMIT_PRESETS, "limitKBps") },
    torrents && { label: "Limit upload speed", title: "How fast this torrent shares while it's still downloading", sub: await limitMenuItems("Limit upload speed", "Upload limit while downloading", "uploadLimitKBps", UPLOAD_PRESETS, "uploadLimitKBps") },
    torrents && { label: "Limit seeding speed", title: "How fast this torrent shares once it's finished", sub: await limitMenuItems("Limit seeding speed", "Upload limit once finished (seeding)", "seedingLimitKBps", UPLOAD_PRESETS, "seedingLimitKBps") },
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
  if (t !== "general" && !(r && r.torrent)) t = "general";
  S.tab = t;
  $$(".tab").forEach((b) => b.classList.toggle("active", b.dataset.tab === t));
  $$(".tabpage").forEach((p) => (p.hidden = p.id !== "tab-" + t));
  refreshDetails();
}

function renderDetails() {
  const r = S.cur ? S.rows.get(S.cur) : null;
  $("#detailsPane").hidden = !r;
  $("#detailsEmpty").hidden = !!r;
  if (!r) { S.details = null; return; }
  if (S.detailsId !== r.id) { S.detailsId = r.id; S.details = null; drawSeg(); drawGraph(); refreshDetails(); }
  $$("[data-torrent]").forEach((e) => (e.hidden = !r.torrent));
  $$("[data-http]").forEach((e) => (e.hidden = r.torrent));
  $$("[data-torrent-only]").forEach((e) => (e.hidden = !r.torrent));
  if (!r.torrent && S.tab !== "general") setTab("general");
  $("#dName").textContent = r.name;
  $("#dName").title = r.name;
  if (document.activeElement !== $("#dUrl")) $("#dUrl").value = r.url;
  $("#dPath").textContent = r.path;
  $("#dProgress").textContent = r.downloadedText + (r.progressText && r.total > 0 ? ` (${r.progressText})` : "");
  $("#dResumable").textContent = r.torrent ? "Yes (torrent)" : r.video ? "Yes (yt-dlp keeps its own partial files)" : r.resumable ? "Yes" : r.total > 0 ? "No — the server doesn't support resuming" : "Not known yet";
  $("#dStatus").textContent = r.statusText;
  $("#dStatus").className = "selectable " + statusClass(r);
  if (document.activeElement !== $("#dConn")) { $("#dConn").value = r.connections; $("#dConnVal").textContent = r.connections; }
  detailLimit.show();
  $$("[data-unfinished]").forEach((e) => (e.hidden = r.status === "Completed"));
  $("#dOpenDone").checked = !!r.openWhenDone;
  $("#dMirrors").textContent = r.mirrors.length ? `${r.mirrors.length} backup link${r.mirrors.length === 1 ? "" : "s"}` : "None";
  if (document.activeElement !== $("#dHash")) $("#dHash").value = r.hash;
  $("#dOpen").disabled = r.status !== "Completed";
  $("#segHint").textContent = r.torrent ? "How much of the wanted files is verified on disk. The Content tab shows each file."
    : r.video ? "yt-dlp downloads the video and audio streams, then joins them with ffmpeg."
    : "Blue = downloaded, green = a live connection writing. Finished connections steal work from slow ones.";
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

let detailsBusy = false;
async function refreshDetails() {
  const r = S.cur ? S.rows.get(S.cur) : null;
  if (!r || detailsBusy || document.hidden) return;
  detailsBusy = true;
  try {
    const d = await invoke("details", { id: r.id });
    if (S.cur !== r.id) return;
    S.details = d;
    const cover = d && d.cover;
    const img = $("#dCover");
    if (cover) { if (img.getAttribute("src") !== cover) img.src = cover; img.hidden = false; } else img.hidden = true;
    $("#segInfo").textContent = d?.segInfo || "";
    if (S.tab === "general") { drawSeg(); drawGraph(); }
    else if (d?.torrent) renderTorrentTab(d.torrent);
  } catch { /* the item went away */ }
  finally { detailsBusy = false; }
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
  const c = $("#segBar");
  if (!c.clientWidth) return;
  const [g, w, h] = fitCanvas(c);
  g.clearRect(0, 0, w, h);
  g.fillStyle = css("--bar-bg");
  g.fillRect(0, 0, w, h);
  const r = S.rows.get(S.cur);
  const segs = S.details?.segments || [];
  if (r && r.status === "Completed") { g.fillStyle = css("--bar-done"); g.fillRect(0, 0, w, h); return; }
  if (!segs.length && r && r.progress > 0) { g.fillStyle = css("--bar"); g.fillRect(0, 0, (w * r.progress) / 100, h); return; }
  for (const [s, , pos, active] of segs) {
    g.fillStyle = css("--bar");
    g.fillRect(s * w, 0, Math.max(0, (pos - s) * w), h);
    if (active) { g.fillStyle = css("--bar-live"); g.fillRect(Math.max(0, pos * w - 2), 0, 3, h); }
  }
  g.fillStyle = css("--panel");
  for (const [s] of segs) if (s > 0) g.fillRect(s * w, 0, 1, h);
}

function drawGraph() {
  const c = $("#graph");
  if (!c.clientWidth) return;
  const [g, w, h] = fitCanvas(c);
  g.clearRect(0, 0, w, h);
  const hist = S.details?.history || [];
  const max = Math.max(1024, ...hist) * 1.15;
  g.strokeStyle = css("--line");
  g.lineWidth = 1;
  for (let i = 1; i < 4; i++) { const y = Math.round((h * i) / 4) + 0.5; g.beginPath(); g.moveTo(0, y); g.lineTo(w, y); g.stroke(); }
  g.fillStyle = css("--text-3");
  g.font = "11px Segoe UI";
  g.fillText(fmtSpeed(max / 1.15), 4, 12);
  if (hist.length < 2) return;
  const n = 120;
  const pts = hist.slice(-n).map((v, i, a) => [w - ((a.length - 1 - i) * w) / (n - 1), h - (v / max) * h]);
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
}

function miniTable(table, grid, cols, rows) {
  const key = grid + cols.map((c) => c[0]).join();
  if (table._key !== key) {
    table._key = key;
    const cg = el("colgroup");
    const tr = el("tr");
    for (const [name, w] of cols) {
      const width = S["w" + grid]?.[name] || w;
      const col = el("col", { style: { width: width + "px" } });
      cg.append(col);
      const th = el("th", { text: name, style: { position: "sticky" } });
      const grip = el("span", { class: "grip", style: { position: "absolute", right: "0", top: "0", bottom: "0", width: "6px", cursor: "col-resize" } });
      grip.addEventListener("mousedown", (e) => {
        e.preventDefault();
        const x0 = e.clientX, w0 = col.getBoundingClientRect().width || width;
        const move = (ev) => { const nw = Math.max(20, w0 + ev.clientX - x0); col.style.width = nw + "px"; (S["w" + grid] ||= {})[name] = Math.round(nw); };
        const up = () => { removeEventListener("mousemove", move); removeEventListener("mouseup", up); saveLayout(); };
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
  if (tb._h !== html) { tb.innerHTML = html; tb._h = html; }
}

function renderTorrentTab(t) {
  if (S.tab === "trackers") {
    miniTable($("#trackerTable"), "Trackers", [["Tier", 44], ["URL", 320], ["Status", 120], ["Seeds", 60], ["Leeches", 66], ["Peers", 56], ["Next announce", 104], ["Message", 260]],
      t.trackers.map((x) => [
        x.tier,
        { html: `<span class="${x.status === "Working" ? "st-Completed" : x.status === "Not working" ? "st-Error" : ""}" title="${esc(x.url)}">${esc(x.url)}${x.mine ? ' <span class="muted">(added by you)</span>' : ""}</span>` },
        x.status, x.seeds, x.leeches, x.peers, x.next, x.message,
      ]));
  } else if (S.tab === "peers") {
    const l = t.live;
    $("#peerSummary").textContent = l ? `Connected ${l.live} (${l.seeds} seeds) · seen ${l.seen} · connecting ${l.connecting} · queued ${l.queued} · dead ${l.dead}` + (l.down ? ` · ↓ ${l.down}` : "") + (l.up ? ` · ↑ ${l.up}` : "") : "Start the torrent to see its peers.";
    if (l?.holepunchActive) $("#peerSummary").textContent += ` · hole punching: ${l.holepunch}`;
    if (l?.turnover) $("#peerSummary").textContent += ` · ${l.turnover} idle peers swapped out`;
    $("#peerSummary").title = l?.holepunch ? "Hole punching (BEP 55) reaches peers behind NAT through a peer you both know: " + l.holepunch : "";
    miniTable($("#peerTable"), "Peers", [["IP/Address", 190], ["Connection", 70], ["Client", 160], ["Progress", 74], ["Down speed", 92], ["Up speed", 92], ["Downloaded", 96], ["Uploaded", 90], ["Flags", 220]],
      t.peers.map((p) => [p.address, p.kind || "", p.client || "", p.progress || "", p.down || "", p.up || "", p.downloaded || "", p.uploaded || "", p.flags || ""]));
  } else if (S.tab === "content") {
    const table = $("#fileTable");
    const prios = ["Don't download", "Low", "Normal", "High", "Maximum"];
    miniTable(table, "Content", [["Download", 80], ["Name", 400], ["Size", 90], ["Priority", 130], ["Progress", 150], ["Remaining", 90]],
      t.files.map((f) => [
        { html: `<input type="checkbox" data-file="${f.index}" ${f.wanted ? "checked" : ""}>` },
        f.path, f.sizeText,
        { html: `<select class="prio" data-prio="${f.index}">${prios.map((p, i) => `<option value="${i}" ${i === f.priority ? "selected" : ""}>${p}</option>`).join("")}</select>` },
        { html: `<div class="cell-prog"><span class="pt">${f.progress.toFixed(1)}%</span><div class="bar ${f.progress >= 100 ? "done" : ""}"><i style="width:${f.progress}%"></i></div></div>` },
        f.remaining,
      ]));
  }
}

// ---------------------------------------------------------------- dialogs: settings

async function openSettings() {
  const g = await invoke("get_settings");
  const s = structuredClone(g.settings);
  if (s.TorrentEnableUtp === undefined) s.TorrentEnableUtp = true;
  if (!(s.TorrentMaxConnectionsPerTorrent > 0)) s.TorrentMaxConnectionsPerTorrent = 100;
  const formats = (await invoke("init")).formats;
  const f = {};
  const text = (key, attrs = {}) => (f[key] = el("input", { type: "text", value: s[key] ?? "", ...attrs }));
  const num = (key, w = 80) => (f[key] = el("input", { type: "number", value: s[key] ?? 0, style: { width: w + "px" } }));
  const chk = (key, label, title) => { const i = el("input", { type: "checkbox" }); i.checked = !!s[key]; f[key] = i; return el("label", { class: "check", title }, i, el("span", { text: label })); };
  const speedBox = (key, allowNone) => { const i = el("input", { type: "text", style: { width: "110px" } }); i.value = s[key] === 0 ? "0" : fmtLimit(s[key]); i.dataset.none = allowNone ? "1" : ""; f[key] = i; return i; };
  const field = (label, ...ctl) => el("div", { class: "field" }, el("span", { text: label }), el("div", { class: "row wrap" }, ...ctl));
  const hint = (t) => el("div", { class: "hint", style: { margin: "4px 0 8px" }, text: t });
  const section = (t) => el("h3", { class: "section", text: t });

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
  const ytStatus = el("span", { class: "hint grow", text: "Checking yt-dlp…" });
  invoke("yt_version").then((v) => (ytStatus.textContent = v));
  const ytBtn = el("button", { text: "Update yt-dlp", onclick: async () => { ytBtn.disabled = true; ytStatus.textContent = "Updating…"; ytStatus.textContent = await invoke("yt_update"); ytBtn.disabled = false; } });

  const fwText = (kind, span) => invoke("firewall_status", { kind }).then((ok) => {
    span.textContent = ok == null ? "" : ok ? "✓ Allowed through Windows Firewall (private networks)" : "Windows Firewall may block incoming connections.";
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

  const themeSel = el("select", {}, ...["System", "Light", "Dark"].map((x) => el("option", { value: x, text: x })));
  themeSel.value = s.Theme || "System";
  const updStatus = el("span", { class: "hint grow" });
  const updBtn = el("button", { text: "Check now", onclick: async () => {
    s.UpdateRepo = f.UpdateRepo.value.trim();
    updStatus.textContent = "Checking…";
    try {
      await saveSettingsQuiet();
      const rel = await invoke("check_update");
      if (rel) { S.update = rel; $("#updateBadge").hidden = false; $("#btnUpdate").textContent = `Update to ${rel.version}`; updStatus.textContent = `Version ${rel.version} is available.`; }
      else updStatus.textContent = "You have the latest version.";
    } catch (e) { updStatus.textContent = String(e); }
  } });

  const body = el("div", {},
    section("Downloads"),
    el("div", { text: "Default save folder" }),
    el("div", { class: "row", style: { margin: "4px 0 8px" } }, text("DefaultFolder", { class: "grow" }), browse),
    chk("CategorySubfolders", "Sort into sub-folders by type (Video, Music, Programs, AI Models…)"),
    el("div", { text: "Default connections per download", style: { marginTop: "10px" } }),
    el("div", { class: "row" }, conn, connVal),
    hint("8–16 is the sweet spot for most servers. Too many can get you rate-limited — FluxDM backs off automatically if that happens."),
    field("Global speed limit", speedBox("GlobalSpeedLimitKBps"), el("span", { class: "hint", text: "e.g. 750k, 2 MB/s — 0 = unlimited" })),

    section("Speed schedule"),
    chk("SpeedScheduleEnabled", "Limit the speed during part of the day"),
    el("div", { class: "row wrap", style: { marginLeft: "23px" } }, "From", text("ScheduleFrom", { style: { width: "70px" } }), "to", text("ScheduleTo", { style: { width: "70px" } }), "limit everything to", speedBox("ScheduleLimitKBps")),
    hint("Outside these hours the normal global limit applies. The window may cross midnight (e.g. 23:00 to 07:00). If both limits apply, the stricter one wins."),

    section("Reliability"),
    field("Retries per connection", num("MaxRetries")),
    field("Reconnect a stalled connection after (s)", num("ReadTimeoutSec")),
    field("Auto-retry failed files in a running queue", num("QueueRetries")),
    el("div", { text: "User-Agent", style: { marginTop: "8px" } }),
    text("UserAgent", { style: { width: "100%", marginTop: "4px" }, class: "mono" }),

    section("Browser & clipboard"),
    chk("ClipboardMonitor", "Watch the clipboard — copying a download link opens the Add dialog"),
    el("div", { text: "File types to catch", style: { marginTop: "8px" } }),
    text("ClipboardExtensions", { style: { width: "100%", marginTop: "4px" } }),
    el("div", { class: "row", style: { marginTop: "12px" } }, chk("LocalApiEnabled", "Browser bridge on 127.0.0.1, port"), num("LocalApiPort")),
    hint("Browser bookmarklets: create a new bookmark and paste one of these as its URL. They only open FluxDM's Add dialog — nothing downloads without your click. Only programs on this PC can reach the bridge."),
    el("b", { text: "“Send to FluxDM” — sends the current page/file URL" }),
    el("div", { class: "row", style: { margin: "4px 0 8px" } }, sendBm, el("button", { text: "Copy", onclick: () => invoke("copy_text", { text: sendBm.value }) })),
    el("b", { text: "“Grab links” — collects every downloadable link on the page" }),
    el("div", { class: "row", style: { margin: "4px 0 8px" } }, grabBm, el("button", { text: "Copy", onclick: () => invoke("copy_text", { text: grabBm.value }) })),

    section("Browser extension"),
    hint("Catches download buttons and links in your browser and sends them here (needs the browser bridge above). Works in Firefox/Waterfox and Chrome/Edge/Brave/Opera/Vivaldi."),
    ext.signed
      ? el("p", { html: "<b>Firefox / Waterfox:</b> click its button, then <b>Add</b> in the browser's prompt." })
      : el("p", { html: "<b>Waterfox / Firefox Developer / ESR:</b> open about:config, set <code>xpinstall.signatures.required</code> to false, then click its button (or about:addons → ⚙ → Install Add-on From File… and pick <b>FluxDM-extension.xpi</b>). Regular Firefox needs the signed build." }),
    geckoButtons.length ? el("div", { class: "row wrap", style: { margin: "4px 0 8px" } }, ...geckoButtons) : hint("No Firefox-family browser found on this PC."),
    el("p", { html: "<b>Chrome / Edge / Brave / Opera / Vivaldi:</b> click its button. It opens the browser's extensions page and the extension folder: turn on <b>Developer mode</b>, click <b>Load unpacked</b> and pick the <b>BrowserExtension</b> folder that just opened." }),
    chromiumButtons.length ? el("div", { class: "row wrap", style: { margin: "4px 0 8px" } }, ...chromiumButtons) : hint("No Chromium browser found on this PC."),
    el("button", { text: "Open extension folder", onclick: () => invoke("open_extension_folder").catch((e) => alertBox("Extension", String(e))) }),

    section("Video downloads (yt-dlp)"),
    hint("For YouTube and 1000+ other sites: tick “Download as video” in the Add window, or right-click a page → Download video with FluxDM. yt-dlp and ffmpeg are fetched once from their official releases on first use and kept in FluxDM's data folder."),
    field("Default quality", fmtSel),
    field("Use browser cookies from", cookieSel),
    hint("Only needed for age-restricted, private or members-only videos. Waterfox works while it's open; Chrome and Edge often lock their cookies."),
    el("div", { class: "row" }, ytStatus, ytBtn),

    section("Torrents (built-in engine)"),
    hint("Paste a magnet link or a link to a .torrent file into Add URL, or use “Open .torrent…” there to add one from disk. Torrents get FluxDM's speed limits, queues, stats and phone control like any other download."),
    field("Listening port (peers + DHT)", num("TorrentPort"), randomPort, el("span", { class: "hint", text: "0 = pick a free one" })),
    chk("TorrentUseUPnP", "Use UPnP port forwarding from my router"),
    chk("TorrentEnableUtp", "Also connect over µTP (UDP), like qBittorrent — reaches more peers (applies after restarting FluxDM)", "µTP peers show “µTP” in the Peers tab. Turn this off if torrents misbehave."),
    field("Maximum connections per torrent", num("TorrentMaxConnectionsPerTorrent"), el("span", { class: "hint", text: "applies after restarting FluxDM" })),
    field("Upload limit while downloading", speedBox("TorrentUploadLimitKBps", true), el("span", { class: "hint", text: "e.g. 50k, 2 MB/s — 0 = unlimited, none = no upload" })),
    field("Seeding limit after it finishes", speedBox("TorrentSeedingLimitKBps", true), el("span", { class: "hint", text: "sharing finished torrents — 0 = unlimited, none = off" })),
    hint("Each is a total shared by all torrents in that state. Right-click a torrent to give it its own limits too — the stricter one wins."),
    field("Keep sharing until ratio", num("TorrentSeedRatio"), el("span", { class: "hint", text: "uploaded ÷ downloaded — 0 = stop seeding right away" })),
    el("div", { class: "row", style: { marginTop: "8px" } }, tFw, fwBtn("torrent", tFw)),
    hint("If this is blocked, other peers can't connect to you — you'll only ever reach the smaller set you happen to connect out to, which usually means fewer peers and slower downloads."),
    el("div", { class: "row", style: { marginTop: "8px" } }, magStatus, magBtn),
    hint("This decides which app opens when you click a magnet: link anywhere on Windows (e.g. in a browser). You can switch back at any time."),

    section("Phone remote"),
    chk("RemoteEnabled", "Control FluxDM from my phone on the home Wi-Fi"),
    el("div", { class: "row wrap", style: { marginLeft: "23px" } }, "Port", num("RemotePort"), "PIN", text("RemotePin", { class: "mono", style: { width: "100px" } }), newPin, pair),
    el("div", { class: "row", style: { marginLeft: "23px", marginTop: "6px" } }, rFw, fwBtn("remote", rFw)),
    hint("Your phone opens a web page served by this PC — no app or account needed. Anyone on your network also needs the PIN; 10 wrong PINs lock that device out for 5 minutes."),

    section("Behaviour"),
    chk("CloseToTray", "Closing the window keeps FluxDM running in the tray"),
    chk("PreventSleep", "Keep the PC awake while downloads are running"),
    chk("ShowIcons", "Show a picture before each download's name (game cover, or the file's own icon)"),
    el("div", { style: { marginLeft: "23px" } }, chk("LookupCoversOnline", "Look up game covers online", "For torrents, installers and archives only. Sends just the cleaned-up title (e.g. “Portal 2”) to Steam's public store search — nothing else.")),
    chk("NotifyOnComplete", "Notify me when a download or queue finishes"),
    chk("StartWithWindows", "Start with Windows (in the tray)"),
    field("Theme", themeSel),

    section("Updates"),
    field("Update source (GitHub owner/repo)", text("UpdateRepo", { class: "mono", style: { width: "260px" }, placeholder: "owner/FluxDM" })),
    chk("AutoCheckUpdates", "Check for a new version when FluxDM starts"),
    el("div", { class: "row" }, updStatus, updBtn),
    el("div", { class: "hint", style: { marginTop: "14px" }, text: `FluxDM ${(await invoke("init")).version} · data folder: ${g.dataDir}` }),
  );

  function collect() {
    const out = structuredClone(s);
    for (const [k, i] of Object.entries(f)) {
      if (i.type === "checkbox") out[k] = i.checked;
      else if (i.type === "number") out[k] = Number(i.value) || 0;
      else if (["GlobalSpeedLimitKBps", "ScheduleLimitKBps", "TorrentUploadLimitKBps", "TorrentSeedingLimitKBps"].includes(k)) {
        const v = parseSpeed(i.value, !!i.dataset.none);
        if (v == null) { i.classList.add("invalid"); i.focus(); throw new Error("Couldn't read that speed. Try 750k or 2 MB/s."); }
        out[k] = v;
      } else out[k] = i.value;
    }
    out.DefaultConnections = +conn.value;
    out.DefaultVideoFormat = fmtSel.value;
    out.VideoCookiesBrowser = cookieSel.value;
    out.Theme = themeSel.value;
    out.TorrentSeedRatio = Number(f.TorrentSeedRatio.value) || 0;
    return out;
  }
  async function saveSettingsQuiet() { await invoke("save_settings", { settings: collect() }); }

  // Section list on the left: jump to a section, and it follows as you scroll.
  const nav = el("div", { class: "settings-nav" });
  const heads = $$("h3.section", body);
  heads.forEach((h) => nav.append(el("button", { class: "ghost", text: h.textContent, onclick: () => h.scrollIntoView({ block: "start", behavior: "smooth" }) })));
  const wrap = el("div", { class: "settings-wrap" }, nav, body);
  body.classList.add("settings-body");
  body.addEventListener("scroll", () => {
    const top = body.scrollTop + 40;
    let cur = 0;
    heads.forEach((h, i) => { if (h.offsetTop <= top) cur = i; });
    [...nav.children].forEach((b, i) => b.classList.toggle("active", i === cur));
  });
  setTimeout(() => body.dispatchEvent(new Event("scroll")), 50);

  modal({
    title: "Settings", body: wrap, xwide: true,
    buttons: [
      { label: "Open data folder", left: true, click: () => { invoke("open_data_folder"); return false; } },
      { label: "Cancel" },
      {
        label: "Save", primary: true,
        click: async () => {
          try {
            const out = collect();
            await invoke("save_settings", { settings: out });
            S.settings = out;
            applyTheme(out.Theme);
            showLimit();
          } catch (e) { alertBox("Settings", e.message || String(e)); return false; }
        },
      },
    ],
  });
}

// ---------------------------------------------------------------- dialogs: scheduler

async function openScheduler(focusName) {
  let queues = await invoke("queues");
  let cur = focusName || (S.nav.startsWith("q:") ? S.nav.slice(2) : queues[0]?.Name);
  let memberSel = null;
  const list = el("div", { class: "sched-list" });
  const right = el("div");
  const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
  const DAYBIT = { Sun: 0, Mon: 1, Tue: 2, Wed: 3, Thu: 4, Fri: 5, Sat: 6 };

  async function reload() { queues = await invoke("queues"); if (!queues.some((q) => q.Name === cur)) cur = queues[0]?.Name; draw(); }

  async function save(q) {
    try { await invoke("queue_save", { oldName: q._old || q.Name, def: q }); delete q._old; return true; }
    catch (e) { alertBox("Scheduler", String(e)); return false; }
  }

  function drawList() {
    list.replaceChildren(...queues.map((q) => {
      const row = el("div", { class: "nav-item" + (q.Name === cur ? " active" : ""), html: icon("queue") });
      row.append(el("span", { class: "ell", text: q.Name }));
      if (q.Running) row.append(el("span", { class: "run-dot", title: "Running" }));
      row.append(el("span", { class: "count", text: q.Members.length || "" }));
      row.addEventListener("click", () => { cur = q.Name; memberSel = null; draw(); });
      return row;
    }));
  }

  function draw() {
    drawList();
    const q = queues.find((x) => x.Name === cur);
    if (!q) { right.replaceChildren(); return; }
    // Editing a field only refreshes the queue list: redrawing the form would drop the cursor mid-edit.
    const commit = () => save(q).then((ok) => ok && drawList());
    const inp = (key, attrs = {}) => { const i = el("input", { type: "text", value: q[key], ...attrs }); i.addEventListener("change", () => { if (key === "Name" && !q._old) q._old = q.Name; q[key] = i.value; if (key === "Name") cur = i.value; commit(); }); return i; };
    const cb = (key, label) => { const i = el("input", { type: "checkbox" }); i.checked = !!q[key]; i.addEventListener("change", () => { q[key] = i.checked; commit(); }); return el("label", { class: "check" }, i, el("span", { text: label })); };
    const max = el("input", { type: "range", min: 1, max: 10, value: q.MaxConcurrent, class: "grow" });
    const maxVal = el("b", { text: q.MaxConcurrent, style: { width: "22px", textAlign: "right" } });
    max.addEventListener("input", () => (maxVal.textContent = max.value));
    max.addEventListener("change", () => { q.MaxConcurrent = +max.value; commit(); });
    const lim = el("input", { type: "text", value: q.SpeedLimitKBps ? fmtLimit(q.SpeedLimitKBps) : "0", style: { width: "110px" }, title: "Shared by all downloads in this queue. KB/s, or 500k / 2 MB/s. 0 = unlimited." });
    lim.addEventListener("change", () => { const v = parseSpeed(lim.value); if (v == null) { lim.classList.add("invalid"); return; } lim.classList.remove("invalid"); q.SpeedLimitKBps = v; commit(); });
    const days = el("div", { class: "days" }, ...DAYS.map((d) => {
      const i = el("input", { type: "checkbox" });
      i.checked = !!(q.DaysMask & (1 << DAYBIT[d]));
      i.addEventListener("change", () => { q.DaysMask = i.checked ? q.DaysMask | (1 << DAYBIT[d]) : q.DaysMask & ~(1 << DAYBIT[d]); commit(); });
      return el("label", {}, i, d);
    }));
    const once = el("input", { type: "date", value: String(q.OnceDate || "").slice(0, 10) });
    once.addEventListener("change", () => { if (once.value) { q.OnceDate = once.value + "T00:00:00"; commit(); } });
    const post = el("select", {}, ...[["None", "Do nothing"], ["Exit", "Exit FluxDM"], ["Sleep", "Sleep"], ["Hibernate", "Hibernate"], ["Shutdown", "Shut down"], ["Restart", "Restart"]].map(([v, t]) => el("option", { value: v, text: t })));
    post.value = q.PostAction;
    post.addEventListener("change", () => { q.PostAction = post.value; commit(); });

    const members = el("div", { class: "members" }, ...(q.Members.length ? q.Members.map((m) => {
      const row = el("div", { class: "m" + (memberSel === m.id ? " sel" : "") }, el("span", { class: "grow ell", text: m.name }), el("span", { class: "hint", text: `${m.size} · ${m.status}` }));
      row.addEventListener("click", () => { memberSel = m.id; draw(); });
      return row;
    }) : [el("div", { class: "m hint", text: "Nothing in this queue yet. Right-click downloads in the main list → Move to queue." })]));
    const mv = (dir) => async () => { if (!memberSel) return; await invoke("queue_move", { name: q.Name, id: memberSel, dir }); reload(); };

    right.replaceChildren(
      el("div", { class: "field" }, el("span", { text: "Queue name" }), inp("Name")),
      el("div", { class: "field" }, el("span", { text: "Downloads at the same time" }), el("div", { class: "row" }, max, maxVal)),
      el("div", { class: "field" }, el("span", { text: "Speed limit for this queue" }), el("div", { class: "row" }, lim, el("span", { class: "hint", text: "shared by its downloads (0 = unlimited)" }))),
      cb("StartOnLaunch", "Start this queue whenever FluxDM starts"),
      el("h3", { class: "section", text: "Start" }),
      el("div", { class: "row" }, cb("StartAtEnabled", "Start the queue at"), inp("StartAt", { style: { width: "80px" }, title: "24-hour time, e.g. 02:00" })),
      cb("Daily", "Repeat on these days"),
      el("div", { style: { marginLeft: "23px", marginBottom: "6px" } }, days),
      el("div", { class: "row" }, el("span", { text: "Otherwise, only once on" }), once),
      el("h3", { class: "section", text: "Stop" }),
      el("div", { class: "row" }, cb("StopAtEnabled", "Stop the queue at"), inp("StopAt", { style: { width: "80px" }, title: "24-hour time, e.g. 08:00" })),
      el("div", { class: "hint", text: "Handy for off-peak data plans: start at night, stop before the morning. Paused files resume exactly where they stopped." }),
      el("h3", { class: "section", text: "When everything in the queue has finished" }),
      post,
      cb("ForceShutdown", "Force apps to close (unsaved work in other apps may be lost)"),
      el("div", { class: "hint", text: "You always get a 60-second countdown you can cancel. An empty queue never triggers this." }),
      el("h3", { class: "section", text: "Files in the queue" }),
      members,
      el("div", { class: "row", style: { marginTop: "8px" } },
        el("button", { class: "small", text: "Move up", onclick: mv(-1) }),
        el("button", { class: "small", text: "Move down", onclick: mv(1) }),
        el("button", { class: "small", text: "Remove from queue", onclick: async () => { if (memberSel) { await invoke("set_items", { ids: [memberSel], patch: { queue: "" } }); memberSel = null; reload(); } } })),
    );
    startBtn.disabled = q.Running;
    stopBtn.disabled = !q.Running;
  }

  const body = el("div", { class: "sched" },
    el("div", { style: { display: "flex", flexDirection: "column", gap: "8px" } },
      el("b", { text: "Queues" }), list,
      el("div", { class: "row" },
        el("button", { class: "grow", text: "New queue", onclick: async () => { cur = await invoke("queue_new"); reload(); } }),
        el("button", { class: "grow", text: "Delete", onclick: async () => {
          if (!(await confirmBox("Delete queue", `Delete the queue “${cur}”? Its downloads stay in the list.`, "Delete", true))) return;
          try { await invoke("queue_delete", { name: cur }); } catch (e) { alertBox("Scheduler", String(e)); }
          reload();
        } }))),
    right);
  const startBtn = el("button", { text: "Start now", onclick: async () => { await invoke("queue_run", { name: cur, on: true }); reload(); } });
  const stopBtn = el("button", { text: "Stop", onclick: async () => { await invoke("queue_run", { name: cur, on: false }); reload(); } });
  const m = modal({ title: "Scheduler", body, xwide: true, buttons: [{ label: "Close", primary: true }] });
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
    g.font = "10.5px Segoe UI";
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
  let n = 60;
  const num = el("div", { class: "countdown-num", text: n });
  const m = modal({
    title: `Queue “${p.queue}” finished`,
    body: el("div", {}, el("p", { text: `${words[p.action] || p.action} in` }), num, el("p", { class: "hint", text: "seconds. Cancel to stay as you are." })),
    buttons: [{ label: "Cancel", primary: true }, { label: "Do it now", value: "now" }],
    onClose: (v) => { clearInterval(t); if (v === "now") invoke("run_post_action", { action: p.action }); },
  });
  const t = setInterval(() => {
    n--;
    num.textContent = n;
    if (n <= 0) { clearInterval(t); m.close("now"); }
  }, 1000);
}

async function offerFirewall(p) {
  const remote = p.kind === "remote";
  const ok = remote
    ? await confirmBox("Phone remote", "Windows Firewall will block your phone from reaching FluxDM.\n\nAllow FluxDM on private (home) networks now? Windows will ask for permission.", "Allow")
    : await confirmBox("Let peers reach you?", "Windows Firewall blocks other BitTorrent peers from connecting to FluxDM, so you only reach the peers you connect out to — usually fewer seeders and slower torrents.\n\nAllow FluxDM on private networks? Windows will ask for permission.", "Allow");
  if (ok) {
    try { await invoke("firewall_allow", { kind: p.kind }); } catch (e) { alertBox("Firewall", String(e)); }
  }
}

async function showUpdate() {
  const rel = S.update;
  if (!rel) return;
  const prog = el("div", { class: "bar", style: { marginTop: "10px" } }, el("i", { id: "updProg" }));
  modal({
    title: `FluxDM ${rel.version} is available`,
    body: el("div", {}, el("p", { text: `You have ${(await invoke("init")).version}. The update downloads ${rel.size ? fmtSize(rel.size) : "the new FluxDM.exe"}, checks its SHA-256 and restarts FluxDM. Running downloads pause and continue after.` }),
      rel.notes ? el("pre", { class: "selectable hint", style: { whiteSpace: "pre-wrap", maxHeight: "240px", overflow: "auto" }, text: rel.notes }) : null, prog),
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

function wire() {
  $("#btnAdd").onclick = () => invoke("open_add", { queue: navQueue() });
  $("#btnResume").onclick = act.resume;
  $("#btnPause").onclick = act.pause;
  $("#btnPauseAll").onclick = act.pauseAll;
  $("#btnDelete").onclick = act.delete;
  $("#btnClear").onclick = act.clearFinished;
  $("#btnScheduler").onclick = () => openScheduler();
  $("#btnStats").onclick = openStats;
  $("#btnColumns").onclick = (e) => { const r = e.currentTarget.getBoundingClientRect(); columnsMenu(r.left, r.bottom + 4); };
  $("#btnSettings").onclick = openSettings;
  $("#btnUpdate").onclick = showUpdate;
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
  $("#dOpen").onclick = act.open;
  $("#dFolder").onclick = act.folder;
  $("#dRefresh").onclick = act.refreshLink;
  $("#dCopy").onclick = () => { const r = S.rows.get(S.cur); if (r) invoke("copy_text", { text: r.url }); };
  $("#dAddMirror").onclick = act.addMirror;
  $("#dConn").addEventListener("input", (e) => ($("#dConnVal").textContent = e.target.value));
  $("#dConn").addEventListener("change", (e) => invoke("set_items", { ids: [S.cur], patch: { connections: +e.target.value } }));
  detailLimit = speedCombo($("#dLimitBox"), {
    get: () => S.rows.get(S.cur)?.limitKBps || 0,
    set: (k) => { const r = S.rows.get(S.cur); if (r) { r.limitKBps = k; invoke("set_items", { ids: [r.id], patch: { limitKBps: k } }); } },
  });
  $("#dOpenDone").addEventListener("change", (e) => { const r = S.rows.get(S.cur); if (r) { r.openWhenDone = e.target.checked; invoke("set_items", { ids: [r.id], patch: { openWhenDone: e.target.checked } }); } });
  const commitHash = () => { const r = S.rows.get(S.cur); if (r && $("#dHash").value.trim() !== r.hash) invoke("set_items", { ids: [r.id], patch: { hash: $("#dHash").value.trim() } }); };
  $("#dHash").addEventListener("keydown", (e) => { if (e.key === "Enter") e.target.blur(); });
  $("#dHash").addEventListener("blur", commitHash);
  $("#fileTable").addEventListener("change", (e) => {
    const i = e.target.dataset.file;
    if (i != null) invoke("torrent_file", { id: S.cur, index: +i, wanted: e.target.checked }).then(refreshDetails);
    const p = e.target.dataset.prio;
    if (p != null) {
      e.target.blur();
      invoke("torrent_file_priority", { id: S.cur, index: +p, level: +e.target.value }).then(refreshDetails);
    }
  });

  // global limit combo
  globalCombo = speedCombo($("#limitBox"), { get: () => S.settings?.GlobalSpeedLimitKBps || 0, set: setGlobalLimit });

  splitter($("#splitSidebar"), (e) => ($("#sidebar").style.width = Math.max(0, Math.min(500, e.clientX)) + "px"));
  splitter($("#splitDetails"), (e) => {
    const r = $("#content").getBoundingClientRect();
    $("#details").style.height = Math.max(0, Math.min(r.height - 40, r.bottom - e.clientY)) + "px";
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
  document.addEventListener("error", (e) => {
    const img = e.target;
    if (img.tagName === "IMG" && img.dataset.cat) img.parentElement.innerHTML = icon(CATEGORY_ICON[img.dataset.cat] || "file");
  }, true);

  document.addEventListener("keydown", async (e) => {
    if ($(".modal-back")) return;
    const inText = e.target.matches("input, textarea, select");
    if (e.ctrlKey && e.key.toLowerCase() === "n") { e.preventDefault(); invoke("open_add", { queue: navQueue() }); return; }
    if (e.ctrlKey && e.key.toLowerCase() === "f") { e.preventDefault(); $("#searchBox").focus(); $("#searchBox").select(); return; }
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
