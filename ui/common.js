// Shared by every FluxDM window: IPC, theme, context menus, small dialogs and icons.
"use strict";

const T = window.__TAURI__;
const invoke = (cmd, args) => T.core.invoke(cmd, args);
const listen = (ev, fn) => T.event.listen(ev, (e) => fn(e.payload));
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

function el(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === "class") e.className = v;
    else if (k === "html") e.innerHTML = v;
    else if (k === "text") e.textContent = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else if (k === "style" && typeof v === "object") Object.assign(e.style, v);
    else e.setAttribute(k, v === true ? "" : v);
  }
  for (const k of kids.flat()) if (k != null && k !== false) e.append(k.nodeType ? k : document.createTextNode(k));
  return e;
}

function applyTheme(theme) {
  const t = (theme || "System").toLowerCase();
  if (t === "light" || t === "dark") document.documentElement.dataset.theme = t;
  else delete document.documentElement.dataset.theme;
}

// ---------------------------------------------------------------- formatting (mirrors the Rust side)

function fmtSize(b) {
  if (b == null || b < 0) return "?";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = b, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${b} B` : `${+v.toFixed(2)} ${u[i]}`;
}
function fmtSpeed(bps) { return bps >= 1 ? `${fmtSize(Math.round(bps))}/s` : ""; }
function fmtLimit(k) {
  if (k < 0) return "No upload";
  if (!k) return "Unlimited";
  return k >= 1024 ? `${+(k / 1024).toFixed(2)} MB/s` : `${k} KB/s`;
}
// Speed-limit presets (KB/s), finer at the low end where a small step matters.
const LIMIT_PRESETS = [["Unlimited", 0], ["64 KB/s", 64], ["128 KB/s", 128], ["192 KB/s", 192], ["256 KB/s", 256], ["384 KB/s", 384], ["512 KB/s", 512], ["768 KB/s", 768],
  ["1 MB/s", 1024], ["1.5 MB/s", 1536], ["2 MB/s", 2048], ["3 MB/s", 3072], ["4 MB/s", 4096], ["5 MB/s", 5120], ["7.5 MB/s", 7680],
  ["10 MB/s", 10240], ["15 MB/s", 15360], ["20 MB/s", 20480], ["25 MB/s", 25600], ["50 MB/s", 51200]];

/** "750k", "2 MB/s", "2.5m", "0", "unlimited" → KB/s (0 = unlimited; -1 = none when allowNone). null if unreadable. */
function parseSpeed(text, allowNone = false) {
  let s = String(text).trim().toLowerCase().replace(/\s/g, "").replace("/s", "").replace("ps", "");
  if (allowNone && ["none", "noupload", "off", "-1"].includes(s)) return -1;
  if (["", "unlimited", "none", "off", "nolimit"].includes(s)) return 0;
  let mult = 1;
  if (/(gb|g)$/.test(s)) { mult = 1048576; s = s.replace(/(gb|g)$/, ""); }
  else if (/(mb|m)$/.test(s)) { mult = 1024; s = s.replace(/(mb|m)$/, ""); }
  else if (/(kb|k)$/.test(s)) s = s.replace(/(kb|k)$/, "");
  const v = Number(s);
  if (!isFinite(v) || v < 0 || s === "") return null;
  return Math.round(v * mult);
}

/**
 * A speed box: type any speed, or pick a preset from the ▾ menu. `box` is a `.speedbox` holding an input and a button.
 * get() returns the current KB/s; set(kbps) is called when the user commits a different, valid value.
 * Returns { show() } to redraw the box from get() (skipped while the user is typing in it).
 */
function speedCombo(box, { get, set, presets = LIMIT_PRESETS }) {
  const input = $("input", box), drop = $("button", box);
  input.spellcheck = false;
  const show = () => { if (document.activeElement !== input) { input.value = fmtLimit(get()); input.classList.remove("invalid"); } };
  const commit = () => {
    const v = parseSpeed(input.value);
    if (v == null) { input.classList.add("invalid"); return false; }
    input.classList.remove("invalid");
    if (v !== get()) set(v);
    input.value = fmtLimit(v);
    return true;
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); if (commit()) input.blur(); }
    if (e.key === "Escape" && document.activeElement === input) { e.stopPropagation(); input.value = fmtLimit(get()); input.blur(); }
  });
  input.addEventListener("blur", () => { if (!commit()) { input.value = fmtLimit(get()); input.classList.remove("invalid"); } });
  input.addEventListener("focus", () => input.select());
  drop.addEventListener("mousedown", (e) => e.preventDefault()); // keep a half-typed value from committing first
  drop.addEventListener("click", () => {
    const r = box.getBoundingClientRect(), cur = get();
    const m = showMenu(presets.map(([l, k]) => ({ label: l, checked: k === cur, click: () => { set(k); input.value = fmtLimit(k); input.classList.remove("invalid"); } })), r.left, r.bottom + 2);
    const h = m.getBoundingClientRect().height;
    if (r.bottom + 2 + h > innerHeight - 4 && r.top - h - 2 >= 4) m.style.top = r.top - h - 2 + "px";
    m.style.minWidth = r.width + "px";
  });
  show();
  return { show };
}

function fmtDuration(s) {
  s = Math.floor(s || 0);
  if (s <= 0) return "";
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), x = s % 60;
  return h ? `${h}h ${m}m` : m ? `${m}m ${x}s` : `${x}s`;
}

// ---------------------------------------------------------------- icons (Fluent-style strokes)

const ICONS = {
  add: '<path d="M8 3v10M3 8h10"/>',
  play: '<path d="M5 3.5v9l7.5-4.5z"/>',
  pause: '<path d="M5.5 3.5v9M10.5 3.5v9"/>',
  pauseAll: '<rect x="3" y="3" width="10" height="10" rx="1.5"/>',
  trash: '<path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5"/>',
  broom: '<path d="M10.5 2.5 7 8M4 9h6l1 4.5H3z"/>',
  clock: '<circle cx="8" cy="8" r="5.5"/><path d="M8 5v3.2l2.2 1.4"/>',
  chart: '<path d="M3 13V8M6.5 13V4M10 13V6.5M13.5 13V9.5"/>',
  columns: '<rect x="2.5" y="3" width="11" height="10" rx="1"/><path d="M6.2 3v10M9.8 3v10"/>',
  gear: '<circle cx="8" cy="8" r="2.2"/><path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.6 3.6l1.1 1.1M11.3 11.3l1.1 1.1M3.6 12.4l1.1-1.1M11.3 4.7l1.1-1.1"/>',
  search: '<circle cx="7" cy="7" r="4.2"/><path d="m10.2 10.2 3.3 3.3"/>',
  all: '<path d="M3 4h10M3 8h10M3 12h10"/>',
  down: '<path d="M8 2.5v8M4.5 7.5 8 11l3.5-3.5M3 13.5h10"/>',
  check: '<path d="m3.5 8.5 3 3 6-7"/>',
  stop: '<rect x="4" y="4" width="8" height="8" rx="1"/>',
  warn: '<path d="M8 2.5 14 13H2z"/><path d="M8 6.5v3M8 11.2v.3"/>',
  queue: '<path d="M3 4h7M3 8h7M3 12h5M12 10v4M10 12h4"/>',
  tag: '<path d="M2.5 8.5V3h5.5l6 6-5.5 5.5z"/><circle cx="5.5" cy="5.5" r=".9"/>',
  folder: '<path d="M2 4.5h4.2l1.3 1.5H14v6.5H2z"/>',
  file: '<path d="M4 2h5l3 3v9H4z"/><path d="M9 2v3h3"/>',
  zip: '<path d="M4 2h8v12H4z"/><path d="M8 2v1M8 4v1M8 6v1M7 8h2v2H7z"/>',
  doc: '<path d="M4 2h5l3 3v9H4z"/><path d="M6 8h4M6 10.5h4"/>',
  music: '<path d="M6 12V4l7-1.5v8"/><circle cx="4.5" cy="12" r="1.5"/><circle cx="11.5" cy="10.5" r="1.5"/>',
  video: '<rect x="2" y="4" width="9" height="8" rx="1"/><path d="m11 7 3-2v6l-3-2"/>',
  app: '<rect x="2.5" y="2.5" width="11" height="11" rx="2"/><path d="M2.5 5.5h11"/>',
  image: '<rect x="2.5" y="3" width="11" height="10" rx="1"/><circle cx="6" cy="6.5" r="1"/><path d="m3 12 3.5-3.5 2.5 2.5 1.5-1.5 2.5 2.5"/>',
  ai: '<rect x="4" y="4" width="8" height="8" rx="1.5"/><path d="M6 1.5V4M10 1.5V4M6 12v2.5M10 12v2.5M1.5 6H4M1.5 10H4M12 6h2.5M12 10h2.5"/>',
  magnet: '<path d="M4 2.5v5a4 4 0 0 0 8 0v-5"/><path d="M4 5h2.5M9.5 5H12M6.5 2.5v5a1.5 1.5 0 0 0 3 0v-5"/>',
  other: '<circle cx="4" cy="8" r=".8"/><circle cx="8" cy="8" r=".8"/><circle cx="12" cy="8" r=".8"/>',
  link: '<path d="M7 9a2.5 2.5 0 0 0 3.5 0l2-2A2.5 2.5 0 0 0 9 3.5l-.8.8"/><path d="M9 7a2.5 2.5 0 0 0-3.5 0l-2 2A2.5 2.5 0 0 0 7 12.5l.8-.8"/>',
  phone: '<rect x="4.5" y="1.5" width="7" height="13" rx="1.5"/><path d="M7 12.5h2"/>',
  x: '<path d="m4 4 8 8M12 4l-8 8"/>',
  copy: '<path d="M5.5 5.5h7v8h-7z"/><path d="M3.5 10.5v-8h7"/>',
  chev: '<path d="m4.5 6.5 3.5 3.5 3.5-3.5"/>',
  up: '<path d="M8 13.5v-8M4.5 8.5 8 5l3.5 3.5M3 2.5h10"/>',
};
function icon(name, extra = "") {
  return `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.35" stroke-linecap="round" stroke-linejoin="round" ${extra}>${ICONS[name] || ICONS.other}</svg>`;
}
const CATEGORY_ICON = { Compressed: "zip", Documents: "doc", Music: "music", Video: "video", Programs: "app", Images: "image", "AI Models": "ai", Torrents: "magnet", Other: "file" };

// ---------------------------------------------------------------- context menus

let openMenus = [];
function closeMenus() { openMenus.forEach((m) => m.remove()); openMenus = []; }
document.addEventListener("mousedown", (e) => { if (!e.target.closest(".menu")) closeMenus(); }, true);
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && openMenus.length) { closeMenus(); e.stopPropagation(); } }, true);
window.addEventListener("blur", closeMenus);

// items: [{label, key, click, disabled, checked, sub:[...], header}] or "-" for a separator
function showMenu(items, x, y, level = 0) {
  if (level === 0) closeMenus();
  const m = el("div", { class: "menu" });
  for (const it of items) {
    if (!it) continue;
    if (it === "-") { m.append(el("div", { class: "sep" })); continue; }
    if (it.header) { m.append(el("div", { class: "mh", text: it.header })); continue; }
    const row = el("div", { class: "mi" + (it.disabled ? " disabled" : ""), title: it.title || null });
    if (items.some((x) => x && x.checked !== undefined)) row.append(el("span", { class: "tick", text: it.checked ? "✓" : "" }));
    row.append(el("span", { text: it.label }));
    if (it.key) row.append(el("span", { class: "key", text: it.key }));
    if (it.sub) {
      row.append(el("span", { class: "arrow", text: "›" }));
      row.addEventListener("mouseenter", () => {
        openMenus.slice(level + 1).forEach((x) => x.remove());
        openMenus = openMenus.slice(0, level + 1);
        $$(".mi.open", m).forEach((x) => x.classList.remove("open"));
        row.classList.add("open");
        const r = row.getBoundingClientRect();
        showMenu(it.sub, r.right - 2, r.top - 4, level + 1);
      });
    } else {
      row.addEventListener("mouseenter", () => {
        openMenus.slice(level + 1).forEach((x) => x.remove());
        openMenus = openMenus.slice(0, level + 1);
        $$(".mi.open", m).forEach((x) => x.classList.remove("open"));
      });
      row.addEventListener("click", () => { closeMenus(); it.click && it.click(); });
    }
    m.append(row);
  }
  document.body.append(m);
  openMenus.push(m);
  const r = m.getBoundingClientRect();
  const W = innerWidth, H = innerHeight;
  if (x + r.width > W - 4) x = level ? Math.max(4, x - r.width - (openMenus[level - 1]?.getBoundingClientRect().width || 0) + 4) : Math.max(4, W - r.width - 4);
  if (y + r.height > H - 4) y = Math.max(4, H - r.height - 4);
  m.style.left = x + "px";
  m.style.top = y + "px";
  return m;
}

// ---------------------------------------------------------------- dialogs

function modal({ title, body, buttons = [], wide = false, xwide = false, onClose, focus = false }) {
  const back = el("div", { class: "modal-back" });
  const box = el("div", { class: "modal" + (wide ? " wide" : "") + (xwide ? " xwide" : "") });
  const bodyEl = el("div", { class: "body" });
  if (typeof body === "string") bodyEl.innerHTML = body; else if (body) bodyEl.append(body);
  const foot = el("footer");
  const close = (v) => { back.remove(); document.removeEventListener("keydown", onKey, true); onClose && onClose(v); };
  for (const b of buttons) {
    const btn = el("button", { class: (b.primary ? "primary" : "") + (b.danger ? " danger" : "") + (b.left ? " left" : ""), text: b.label });
    btn.addEventListener("click", async () => {
      if (b.click) {
        const r = await b.click(btn);
        if (r === false) return;
      }
      close(b.value);
    });
    foot.append(btn);
  }
  const onKey = (e) => {
    if (e.key === "Escape" && !openMenus.length) { e.preventDefault(); close(undefined); }
    if (e.key === "Enter" && e.target.tagName !== "TEXTAREA" && e.target.tagName !== "BUTTON") {
      const p = buttons.findIndex((b) => b.primary);
      if (p >= 0) { e.preventDefault(); foot.children[p].click(); }
    }
  };
  document.addEventListener("keydown", onKey, true);
  box.append(el("header", { text: title }), bodyEl, foot);
  back.append(box);
  back.addEventListener("mousedown", (e) => { if (e.target === back) back.dataset.down = "1"; });
  back.addEventListener("mouseup", (e) => { if (e.target === back && back.dataset.down) close(undefined); delete back.dataset.down; });
  document.body.append(back);
  // Prompts put the cursor in their box; bigger dialogs focus their main button instead.
  setTimeout(() => {
    const f = focus && $("input, textarea, select", bodyEl);
    if (f) { f.focus(); f.select && f.select(); }
    else { const p = $("button.primary", foot); if (p) p.focus({ focusVisible: false }); }
  }, 30);
  return { back, box, body: bodyEl, close };
}

function alertBox(title, text) {
  return new Promise((res) => modal({ title, body: el("p", { class: "selectable", style: { whiteSpace: "pre-wrap" }, text }), buttons: [{ label: "OK", primary: true }], onClose: res }));
}

function confirmBox(title, text, okLabel = "OK", danger = false) {
  return new Promise((res) =>
    modal({
      title, body: el("p", { style: { whiteSpace: "pre-wrap" }, text }),
      buttons: [{ label: "Cancel", value: false }, { label: okLabel, primary: !danger, danger, value: true }],
      onClose: (v) => res(!!v),
    }));
}

// Returns the text, or null when cancelled.
function promptBox(title, label, value = "", { multiline = false, mono = false, hint = "", validate } = {}) {
  return new Promise((res) => {
    const input = multiline ? el("textarea", { rows: 6, class: mono ? "mono" : "" }) : el("input", { type: "text", class: mono ? "mono" : "" });
    input.value = value;
    input.style.width = "100%";
    const err = el("div", { class: "err-text hint", style: { minHeight: "18px", marginTop: "4px" } });
    const body = el("div", {}, label ? el("p", { text: label }) : null, input, hint ? el("div", { class: "hint", style: { marginTop: "6px" }, text: hint }) : null, err);
    let result = null;
    modal({
      title, body, wide: multiline, focus: true,
      buttons: [
        { label: "Cancel" },
        {
          label: "OK", primary: true,
          click: async () => {
            const v = input.value;
            if (validate) {
              const msg = await validate(v);
              if (msg) { err.textContent = msg; return false; }
            }
            result = v;
          },
        },
      ],
      onClose: () => res(result),
    });
  });
}

// Blocks right-click/refresh/zoom behaviour that doesn't belong in an app window.
document.addEventListener("contextmenu", (e) => {
  if (!e.target.closest("input, textarea, .selectable")) e.preventDefault();
});
document.addEventListener("keydown", (e) => {
  if (e.key === "F5" || (e.ctrlKey && (e.key === "r" || e.key === "R" || e.key === "p" || e.key === "P"))) e.preventDefault();
});
