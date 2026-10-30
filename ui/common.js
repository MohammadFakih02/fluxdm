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

// Theme remains readable for older settings; desktop surfaces are always dark.
function applyTheme() { document.documentElement.dataset.theme = "dark"; }

/** App icon themes (Settings → Appearance), matching ICON_THEMES in main.rs: the hue the gold parts take, how
 *  much colour they keep, and an optional tile tint [hue, added saturation]. Light and shade stay the original's. */
const ICON_THEMES = [
  ["heat", "Full heat", null],
  ["ice", "Ice", [195, .9, [220, .18]]],
  ["neon", "Neon", [320, 1, [275, .2]]],
  ["emerald", "Emerald", [152, .9, [160, .15]]],
  ["mono", "Mono", [30, .04, null]],
];
function rgbHsv(r, g, b) {
  const max = Math.max(r, g, b), d = max - Math.min(r, g, b);
  const h = d === 0 ? 0 : max === r ? 60 * ((((g - b) / d) % 6 + 6) % 6) : max === g ? 60 * ((b - r) / d + 2) : 60 * ((r - g) / d + 4);
  return [h, max > 0 ? d / max : 0, max];
}
function hsvRgb(h, s, v) {
  const c = v * s, x = c * (1 - Math.abs(((h / 60) % 2 + 2) % 2 - 1)), m = v - c, i = Math.floor((((h % 360) + 360) % 360) / 60) % 6;
  const [r, g, b] = [[c, x, 0], [x, c, 0], [0, c, x], [0, x, c], [x, 0, c], [c, 0, x]][i];
  return [r + m, g + m, b + m];
}
/** Same steps as `tint_icon` in main.rs, so the in-app logo matches the tray and taskbar icon. */
function tintPixels(px, theme) {
  const t = ICON_THEMES.find(([id]) => id === theme)?.[2];
  if (!t) return;
  const [hue, sat, tile] = t;
  for (let i = 0; i < px.length; i += 4) {
    if (!px[i + 3]) continue;
    const [h, s, v] = rgbHsv(px[i] / 255, px[i + 1] / 255, px[i + 2] / 255);
    let out = null;
    if (s >= .18 && (h < 75 || h > 330)) out = hsvRgb(hue, Math.min(1, s * sat), v);
    else if (s < .18 && v < .5 && tile) out = hsvRgb(tile[0], Math.min(1, s + tile[1]), v);
    if (out) { px[i] = Math.round(out[0] * 255); px[i + 1] = Math.round(out[1] * 255); px[i + 2] = Math.round(out[2] * 255); }
  }
}
const themedLogos = new Map();
/** The logo in a theme, as a picture URL (worked out once per theme). */
function themedLogo(theme) {
  if (!ICON_THEMES.some(([id, , t]) => id === theme && t)) return Promise.resolve("icon.png");
  if (!themedLogos.has(theme)) themedLogos.set(theme, (async () => {
    try {
      const img = new Image(); img.src = "icon.png"; await img.decode();
      const c = document.createElement("canvas"); c.width = img.naturalWidth; c.height = img.naturalHeight;
      const g = c.getContext("2d", { willReadFrequently: true }); g.drawImage(img, 0, 0);
      const d = g.getImageData(0, 0, c.width, c.height); tintPixels(d.data, theme); g.putImageData(d, 0, 0);
      return c.toDataURL("image/png");
    } catch { return "icon.png"; }
  })());
  return themedLogos.get(theme);
}
let logoUrl = "icon.png", logoTheme = "heat";
function appLogo() { return logoUrl; }
async function updateLogo(theme = "heat") {
  logoTheme = theme || "heat";
  const url = await themedLogo(logoTheme);
  if (logoTheme !== (theme || "heat")) return;
  logoUrl = url;
  document.querySelectorAll?.("img.app-logo")?.forEach((i) => { i.src = url; });
}
function applyAppearance(settings = {}) {
  applyTheme();
  const root = document.documentElement;
  updateLogo(settings.IconTheme).catch(() => {});
  // Background art: how strongly it shows (100 = the measured, faint default) and an optional fixed blur.
  const strength = Math.max(25, Math.min(500, Number(settings.ArtStrength) || 100));
  root.style.setProperty("--art-strength", String(strength / 100));
  const blur = Number(settings.ArtBlur);
  const bannerStrength = Math.max(25, Math.min(150, Number(settings.BannerStrength) || 100));
  root.style.setProperty("--banner-strength", String(bannerStrength / 100));
  const bannerBlur = Math.max(0, Math.min(40, Number(settings.BannerBlur) || 0));
  root.style.setProperty("--banner-blur", bannerBlur + "px");
  root.style.setProperty("--banner-blur-scale", String(bannerBlur ? .06 : 0));
  const cardStrength = Math.max(25, Math.min(400, Number(settings.CardArtStrength) || 100));
  root.style.setProperty("--card-art-strength", String(cardStrength / 100));
  if (typeof updateCardText === "function") updateCardText(cardStrength / 100);
  const cardBlur = Math.max(0, Math.min(40, Number(settings.CardArtBlur) || 0));
  root.style.setProperty("--card-art-blur", cardBlur + "px");
  root.style.setProperty("--card-art-blur-scale", String(cardBlur ? .08 : 0)); // hides the soft blurred edges
  if (settings.ArtBlur != null && Number.isFinite(blur) && blur >= 0) root.style.setProperty("--art-user-blur", Math.min(80, blur) + "px");
  else root.style.removeProperty?.("--art-user-blur");
  const backgrounds = { navy: "#0b1222", black: "#000000", graphite: "#16181c", purple: "#150f22", forest: "#0b1a14" };
  root.style.setProperty("--bg", backgrounds[settings.BackgroundColor] || backgrounds.navy);
  const accent = /^#[0-9a-f]{6}$/i.test(settings.AccentColor || "") ? settings.AccentColor : "#e9c98f";
  root.style.setProperty("--accent", accent);
  root.style.setProperty("--line-glow", String(Math.max(20, Math.min(100, Number(settings.LineGlow) || 70)) / 100));
  root.dataset.glass = ["solid", "frosted", "clear", "ghost"].includes(settings.Glass) ? settings.Glass : "ghost";
  root.dataset.accentFromArt = String(settings.AccentFromArt !== false);
  root.dataset.artBehindWindow = String(settings.ArtBehindWindow !== false);
  root.dataset.artSelectedOnly = String(settings.ArtSelectedOnly !== false);
  // Native select arrows use the same paths as the other disclosure controls.
  root.style.setProperty("--select-arrow", 'url("data:image/svg+xml,' + encodeURIComponent(icon("chev").replace('<svg ', '<svg xmlns="http://www.w3.org/2000/svg" ').replace('currentColor', '#a3abb8')) + '")');
}

// Registered before each page requests its initial settings, including hidden toast windows.
if (T.event?.listen) {
  listen("theme-changed", applyTheme);
  listen("appearance-changed", applyAppearance);
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
// Torrent upload presets (KB/s; -1 = no upload at all).
const UPLOAD_PRESETS = [["Unlimited", 0], ["No upload", -1], ["20 KB/s", 20], ["50 KB/s", 50], ["100 KB/s", 100], ["128 KB/s", 128], ["192 KB/s", 192], ["256 KB/s", 256], ["384 KB/s", 384], ["512 KB/s", 512], ["768 KB/s", 768], ["1 MB/s", 1024], ["1.5 MB/s", 1536], ["2 MB/s", 2048], ["3 MB/s", 3072], ["5 MB/s", 5120], ["10 MB/s", 10240]];

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
 * allowNone: "No upload" (-1) is a valid value (upload limits).
 */
function speedCombo(box, { get, set, presets = LIMIT_PRESETS, allowNone = false }) {
  const input = $("input", box), drop = $("button", box);
  drop.innerHTML = icon("chev");
  input.spellcheck = false;
  const show = () => { if (document.activeElement !== input) { input.value = fmtLimit(get()); input.classList.remove("invalid"); } };
  const commit = () => {
    const v = parseSpeed(input.value, allowNone);
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

// ---------------------------------------------------------------- icons (24px outline set)

const ICONS = {
  refresh: '<path d="M21 12a9 9 0 1 1-3-6.7L21 8M21 3v5h-5"/>',
  palette: '<path d="M12 3a9 9 0 1 0 0 18c2 0 2-2 1-3s0-3 2-3h2a4 4 0 0 0 4-4c0-4-4-8-9-8zM7 12h.01M10 7h.01M15 7h.01"/>',
  shield: '<path d="m12 3 8 4v5c0 5-4 8-8 9-4-1-8-4-8-9V7z"/>',
  browser: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M7 6h.01M10 6h.01"/>',
  extension: '<path d="M14 4h4v4a2 2 0 1 0 0 4v4h-4a2 2 0 1 0-4 0H6v-4a2 2 0 1 0 0-4V4h4a2 2 0 1 0 4 0z"/>',
  phone: '<rect x="6" y="2" width="12" height="20" rx="2"/><path d="M11 18h2"/>',
  sliders: '<path d="M4 3v7m0 4v7M12 3v5m0 4v9M20 3v13m0 4v1M1 10h6M9 8h6M17 16h6"/>',
  wrench: '<path d="m14 6 4 4 3-3a6 6 0 0 1-8 7l-7 7-3-3 7-7a6 6 0 0 1 7-8z"/>',
  add: '<path d="M12 5v14M5 12h14"/>',
  play: '<path d="m9 5 12 7-12 7z"/>',
  pause: '<path d="M9 5v14M15 5v14"/>',
  pauseAll: '<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M9 8v8M15 8v8"/>',
  trash: '<path d="M3 6h18M9 6V4h6v2M5 6l1 14h12l1-14M10 10v6M14 10v6"/>',
  broom: '<path d="m15 3-5 9M5 12h10l3 9H2zM8 16l-1 5M12 16l1 5"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  chart: '<path d="M3 3v18h18M8 17v-3M13 17V5M18 17V9"/>',
  columns: '<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M9 3v18M15 3v18"/>',
  gear: '<path d="M12.00 1.00L14.15 1.21L15.25 4.15L16.72 4.93L19.78 4.22L21.15 5.89L19.85 8.75L20.34 10.34L23.00 12.00L22.79 14.15L19.85 15.25L19.07 16.72L19.78 19.78L18.11 21.15L15.25 19.85L13.66 20.34L12.00 23.00L9.85 22.79L8.75 19.85L7.28 19.07L4.22 19.78L2.85 18.11L4.15 15.25L3.66 13.66L1.00 12.00L1.21 9.85L4.15 8.75L4.93 7.28L4.22 4.22L5.89 2.85L8.75 4.15L10.34 3.66z"/><circle cx="12" cy="12" r="3"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-4-4"/>',
  all: '<path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/>',
  down: '<path d="M12 3v12m-5-5 5 5 5-5M5 17v4h14v-4"/>',
  check: '<path d="m5 12 4 4L19 6"/>',
  stop: '<rect x="5" y="5" width="14" height="14" rx="2"/>',
  warn: '<path d="m12 3 10 18H2zM12 9v4M12 17h.01"/>',
  wifiOff: '<path d="m2 2 20 20M16.7 11.1a11 11 0 0 1 2.3 1.4M5 12.5a11 11 0 0 1 5.2-2.4M10.7 5.1A16 16 0 0 1 22.6 9M1.4 9a16 16 0 0 1 4.7-2.9M8.5 16.1a5 5 0 0 1 7 0M12 20h.01"/>',
  queue: '<path d="M3 6h12M3 12h12M3 18h8m6-3 4 3-4 3z"/>',
  tag: '<path d="M3 3h7l11 11-7 7L3 10z"/><circle cx="7" cy="7" r="1"/>',
  folder: '<path d="M3 7V4h6l2 3h10v13H3z"/>',
  file: '<path d="M5 3h9l5 5v13H5zM14 3v5h5"/>',
  zip: '<rect x="5" y="3" width="14" height="18" rx="2"/><path d="M12 3v2M12 7v2M12 11v2M11 15h2v3h-2z"/>',
  doc: '<path d="M5 3h9l5 5v13H5zM14 3v5h5M9 12h6M9 16h6"/>',
  music: '<path d="M9 18V5l12-2v13M9 9l12-2"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>',
  video: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 5v14M17 5v14M3 10h4M3 14h4M17 10h4M17 14h4"/>',
  app: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M7 6.5h.01M10 6.5h.01"/>',
  image: '<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8" cy="8" r="1"/><path d="m3 17 5-5 4 4 3-3 6 6"/>',
  ai: '<rect x="5" y="5" width="14" height="14" rx="2"/><path d="M9 3v2M15 3v2M9 19v2M15 19v2M3 9h2M3 15h2M19 9h2M19 15h2M9 9h6v6H9z"/>',
  game: '<path d="M6 11h4M8 9v4M15 12h.01M18 10h.01M7 5h10a4 4 0 0 1 4 4l1 6a3 3 0 0 1-5 3l-2-3H9l-2 3a3 3 0 0 1-5-3l1-6a4 4 0 0 1 4-4z"/>',
  magnet: '<path d="m6 15-4-4 7-7a8 8 0 0 1 11 11l-7 7-4-4 7-7a2 2 0 0 0-3-3zM5 8l4 4M12 15l4 4"/>',
  other: '<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>',
  link: '<path d="M10 13a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-2 2M14 11a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l2-2"/>',
  phone: '<rect x="6" y="2" width="12" height="20" rx="2"/><path d="M11 18h2"/>',
  x: '<path d="m6 6 12 12M18 6 6 18"/>',
  copy: '<rect x="8" y="8" width="13" height="13" rx="2"/><path d="M16 8V3H3v13h5"/>',
  chev: '<path d="m6 9 6 6 6-6"/>',
  chevRight: '<path d="m9 6 6 6-6 6"/>',
  arrowUp: '<path d="M12 19V5m-6 6 6-6 6 6"/>',
  arrowDown: '<path d="M12 5v14m-6-6 6 6 6-6"/>',
  up: '<path d="M12 21V9m-5 5 5-5 5 5M5 7V3h14v4"/>',
};
function icon(name, extra = "") {
  return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false" ${extra}>${ICONS[name] || ICONS.other}</svg>`;
}
const CATEGORY_ICON = { Compressed: "zip", Documents: "doc", Music: "music", Video: "video", Programs: "app", Images: "image", "AI Models": "ai", Torrents: "magnet", Other: "file" };

// ---------------------------------------------------------------- context menus

let openMenus = [];
function closeMenus() { openMenus.forEach((m) => m.remove()); openMenus = []; }
document.addEventListener("mousedown", (e) => { if (!e.target.closest(".menu")) closeMenus(); }, true);
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && openMenus.length) { closeMenus(); e.stopImmediatePropagation(); } }, true);
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
    if (items.some((x) => x && x.checked !== undefined)) row.append(el("span", { class: "tick", html: it.checked ? icon("check") : "" }));
    row.append(el("span", { text: it.label }));
    if (it.key) row.append(el("span", { class: "key", text: it.key }));
    if (it.sub) {
      row.append(el("span", { class: "arrow", html: icon("chevRight") }));
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

const modals = [];
let modalId = 0;

function modal({ title, body, buttons = [], wide = false, xwide = false, onClose, beforeClose, focus = false, backdrop = true }) {
  const previousFocus = document.activeElement;
  const back = el("div", { class: "modal-back" });
  const titleId = `modal-title-${++modalId}`;
  const box = el("div", { class: "modal" + (wide ? " wide" : "") + (xwide ? " xwide" : ""), role: "dialog", "aria-modal": "true", "aria-labelledby": titleId, tabindex: -1 });
  const bodyEl = el("div", { class: "body" });
  if (typeof body === "string") bodyEl.innerHTML = body; else if (body) bodyEl.append(body);
  const foot = el("footer");
  let primaryButton;
  let closed = false;
  let checkingClose = false;
  const close = async (v) => {
    if (closed || checkingClose) return;
    if (beforeClose) {
      checkingClose = true;
      try { if (!await beforeClose(v)) return; } finally { checkingClose = false; }
    }
    closed = true;
    const top = modals.at(-1) === back;
    modals.splice(modals.indexOf(back), 1);
    back.remove();
    document.removeEventListener("keydown", onKey);
    if (top && previousFocus?.isConnected) previousFocus.focus();
    onClose && onClose(v);
  };
  for (const b of buttons) {
    const btn = el("button", { class: (b.primary ? "primary" : "") + (b.danger ? " danger" : "") + (b.left ? " left" : ""), text: b.label });
    if (b.primary) primaryButton = btn;
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
    if (modals.at(-1) !== back || e.defaultPrevented || e.cancelBubble) return;
    if (e.key === "Tab") {
      const controls = $$("button, input, textarea, select, a[href], [tabindex]", box)
        .filter((c) => !c.disabled && c.tabIndex >= 0 && c.getClientRects().length);
      const first = controls[0], last = controls.at(-1);
      if (!first || !controls.includes(document.activeElement) || (e.shiftKey ? document.activeElement === first : document.activeElement === last)) {
        e.preventDefault();
        (e.shiftKey ? last || box : first || box).focus();
      }
      e.stopImmediatePropagation();
      return;
    }
    if (e.key !== "Escape" && e.key !== "Enter") return;
    e.stopImmediatePropagation();
    if (e.key === "Escape" && !openMenus.length) { e.preventDefault(); close(undefined); }
    if (e.key === "Enter" && (!box.contains(e.target) || (e.target.tagName !== "TEXTAREA" && e.target.tagName !== "BUTTON"))) {
      e.preventDefault();
      if (primaryButton && !primaryButton.disabled) primaryButton.click();
    }
  };
  document.addEventListener("keydown", onKey);
  box.append(el("header", {}, el("span", { id: titleId, text: title }),
    el("button", { class: "ghost modal-close", "aria-label": "Close dialog", html: icon("x"), onclick: () => close(undefined) })), bodyEl, foot);
  back.append(box);
  back.addEventListener("mousedown", (e) => { if (e.target === back) back.dataset.down = "1"; });
  back.addEventListener("mouseup", (e) => {
    if (modals.at(-1) === back && e.target === back && back.dataset.down && backdrop && !$("input, textarea, select, form, [contenteditable]", bodyEl)) close(undefined);
    delete back.dataset.down;
  });
  document.body.append(back);
  modals.push(back);
  // Prompts put the cursor in their box; bigger dialogs focus their main button instead.
  setTimeout(() => {
    if (closed || modals.at(-1) !== back) return;
    const f = focus && $("input, textarea, select", bodyEl);
    if (f) { f.focus(); f.select && f.select(); }
    else { const p = $("button.primary", foot) || $("button", foot) || box; p.focus({ focusVisible: false }); }
  }, 30);
  return { back, box, body: bodyEl, close };
}

function alertBox(title, text) {
  return new Promise((res) => modal({ title, body: el("p", { class: "selectable", style: { whiteSpace: "pre-wrap" }, text }), buttons: [{ label: "OK", primary: true }], onClose: res }));
}

function confirmBox(title, text, okLabel = "Confirm", danger = false) {
  return new Promise((res) =>
    modal({
      title, body: el("p", { style: { whiteSpace: "pre-wrap" }, text }),
      buttons: [{ label: "Cancel", value: false }, { label: okLabel, primary: !danger, danger, value: true }],
      onClose: (v) => res(!!v),
    }));
}

// Returns the text, or null when cancelled.
function promptBox(title, label, value = "", { multiline = false, mono = false, hint = "", validate, action = "Save" } = {}) {
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
          label: action, primary: true,
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

function artAccent(pixels) {
  const buckets = new Map();
  for (let i = 0; i < pixels.length; i += 4) {
    const [r, g, b] = pixels.slice(i, i + 3), light = .2126 * r + .7152 * g + .0722 * b;
    if (light < 100 || light > 240 || Math.max(r, g, b) - Math.min(r, g, b) < 18) continue;
    const key = `${r >> 5},${g >> 5},${b >> 5}`;
    const bucket = buckets.get(key) || [0, 0, 0, 0];
    bucket[0]++; bucket[1] += r; bucket[2] += g; bucket[3] += b; buckets.set(key, bucket);
  }
  const best = [...buckets.values()].sort((a, b) => b[0] - a[0])[0];
  if (!best) return null;
  const rgb = best.slice(1).map(v => v / best[0]);
  const max = Math.max(...rgb), min = Math.min(...rgb), delta = max - min;
  let hue = max === rgb[0] ? ((rgb[1]-rgb[2])/delta+6)%6 : max === rgb[1] ? (rgb[2]-rgb[0])/delta+2 : (rgb[0]-rgb[1])/delta+4;
  // Bright yellow banners keep a warm gold accent rather than a pale lemon tint.
  if (hue >= .8 && hue <= 1.3) hue = .8;
  const light = Math.max(.70, Math.min(.76, (max+min)/510));
  const saturation = Math.max(.22, Math.min(.58, delta/255/(1-Math.abs((max+min)/255-1))));
  const c = (1-Math.abs(2*light-1))*saturation, x = c*(1-Math.abs(hue%2-1)), m = light-c/2;
  const channels = [[c,x,0],[x,c,0],[0,c,x],[0,x,c],[x,0,c],[c,0,x]][Math.floor(hue)%6];
  return "#" + channels.map(v => Math.round((v+m)*255).toString(16).padStart(2,"0")).join("");
}
/** Background art brightness when a picture can't be measured (e.g. a web image that blocks reading its pixels). */
const ART_FALLBACK_BRIGHTNESS = .1;
function artTreatment(pixels) {
  let luminance = 0;
  for (let i=0; i<pixels.length; i+=4) luminance += .2126*pixels[i]+.7152*pixels[i+1]+.0722*pixels[i+2];
  luminance /= Math.max(1,pixels.length/4);
  // Keep average artwork near the base surface: bright banners are dimmed, and dark ones (night scenes, horror
  // games) are lifted to the same level instead of staying near-black. luminance also adapts the selected card's text.
  return { luminance, accent: artAccent(pixels), brightness: Math.min(.7,9/Math.max(1,luminance)), cardBrightness: Math.min(.85,110/Math.max(1,luminance)), blur: 34+14*luminance/255 };
}

const surfaceArtCache = new Map();
let surfaceArtGeneration = 0;
function applySurfaceArt(banner, settings = {}, background = null) {
  const generation = ++surfaceArtGeneration, root = document.documentElement;
  const fallback = /^#[0-9a-f]{6}$/i.test(settings.AccentColor || "") ? settings.AccentColor : "#e9c98f";
  if (background) background.style.backgroundImage = banner && settings.ArtBehindWindow !== false ? `url(${JSON.stringify(banner)})` : "none";
  const apply = value => {
    root.style.setProperty("--accent", settings.AccentFromArt !== false ? value?.accent || fallback : fallback);
    root.style.setProperty("--art-brightness", String(value?.brightness ?? ART_FALLBACK_BRIGHTNESS));
    root.style.setProperty("--art-card-brightness", String(value?.cardBrightness ?? .65));
    root.style.setProperty("--art-adaptive-blur", (value?.blur ?? 48) + "px");
  };
  apply(null);
  if (!banner) return;
  if (!surfaceArtCache.has(banner)) surfaceArtCache.set(banner, new Promise(resolve => {
    const img = new Image(); img.crossOrigin = "anonymous";
    img.onload = () => { try {
      const c = document.createElement("canvas"); c.width = 48; c.height = 24;
      const ctx = c.getContext("2d", { willReadFrequently: true }); ctx.drawImage(img,0,0,48,24);
      resolve(artTreatment(ctx.getImageData(0,0,48,24).data));
    } catch { resolve(null); } };
    img.onerror = () => resolve(null); img.src = banner;
  }));
  surfaceArtCache.get(banner).then(value => { if (generation === surfaceArtGeneration) apply(value); });
}

function fallbackArt(glyph = "file") {
  return "data:image/svg+xml," + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect width="24" height="24" fill="#16334b"/><g fill="none" stroke="#8fd8ff" stroke-width="1.5">' + icon(glyph).replace(/^.*?<svg[^>]*>/, "").replace(/<\/svg>.*$/, "") + '</g></svg>');
}
function setCardArt(element, banner, picture, glyph, color) {
  element.dataset.artColor = color || "";
  const url = banner || picture || fallbackArt(glyph);
  element.style.backgroundImage = 'url(' + JSON.stringify(url) + ')';
  element.classList.toggle("icon-wash", !banner);
  sampleCardArt(element, url);
  return url;
}

// Where the picture's subject is: the centre of its edge detail (flat sky and fades carry little),
// used as the crop focus so cropping keeps faces and characters in view.
const bannerFocusCache = new Map();
function bannerFocus(url) {
  if (typeof Image !== "function") return Promise.resolve(null);
  if (!bannerFocusCache.has(url)) bannerFocusCache.set(url, new Promise(resolve => {
    const img = new Image(); img.crossOrigin = "anonymous";
    img.onload = () => { try {
      const W = 64, H = 36, c = document.createElement("canvas"); c.width = W; c.height = H;
      const ctx = c.getContext("2d", { willReadFrequently: true }); ctx.drawImage(img, 0, 0, W, H);
      const d = ctx.getImageData(0, 0, W, H).data, lum = i => d[i] * .3 + d[i + 1] * .59 + d[i + 2] * .11;
      let sx = 0, sy = 0, sw = 0;
      for (let y = 1; y < H - 1; y++) for (let x = 1; x < W - 1; x++) {
        const i = (y * W + x) * 4, e = Math.abs(lum(i + 4) - lum(i - 4)) + Math.abs(lum(i + W * 4) - lum(i - W * 4)), w = e * e;
        sx += x * w; sy += y * w; sw += w;
      }
      if (!sw) return resolve(null);
      const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
      resolve({ x: Math.round(clamp(sx / sw / (W - 1) * 100, 15, 85)), y: Math.round(clamp(sy / sw / (H - 1) * 100, 15, 70)) });
    } catch { resolve(null); } };
    img.onerror = () => resolve(null); img.src = url;
  }));
  return bannerFocusCache.get(url);
}
const bannerSizeCache = new Map();
/** Sizes already measured, so a known banner is laid out in the same frame (no remove-then-re-add flash). */
const bannerSizeKnown = new Map();
/** Decoded pictures, kept so switching back to a download shows its art at once instead of a blank frame. */
const decodedArt = new Map();
/** Loads and decodes a picture before it's shown; also records its size. Resolves when it's ready (or failed). */
function preloadArt(url) {
  if (!url || typeof Image !== "function") return Promise.resolve();
  if (!decodedArt.has(url)) {
    const img = new Image();
    img.src = url;
    decodedArt.set(url, (img.decode ? img.decode() : Promise.reject()).then(() => {
      bannerSizeKnown.set(url, { width: img.naturalWidth, height: img.naturalHeight });
      return img;
    }).catch(() => null));
    if (decodedArt.size > 60) decodedArt.delete(decodedArt.keys().next().value);
  }
  return decodedArt.get(url);
}
function sizeCardBanner(element, url) {
  if (typeof Image !== "function") return;
  const isBanner = !element.classList.contains("icon-wash") && !element.querySelector(".icon-wash");
  // Same picture as last time: everything is already in place (this runs on every details refresh).
  if (element.dataset.bannerUrl === url && element.dataset.bannerLaid === String(isBanner)) return;
  element.dataset.bannerUrl = url;
  const apply = (size) => {
    element.dataset.bannerLaid = String(isBanner);
    element.classList.toggle("aspect-banner", !!size && size.width >= 1000);
    element.classList.toggle("fills", !!size && size.width / size.height >= 2.5);
    element.classList.toggle("small-banner", !!size && size.width > 0 && size.width < 1000);
    if (!size) { if (element.parentElement) element.parentElement.style.removeProperty("--banner-ratio"); return; }
    if (typeof bannerFocus === "function") bannerFocus(url).then(f => { if (f && element.dataset.bannerUrl === url) { element.style.setProperty("--focus-x", f.x + "%"); element.style.setProperty("--focus-y", f.y + "%"); } });
    if (element.parentElement) element.parentElement.style.setProperty("--banner-ratio", size.width / size.height);
    element.style.setProperty("--banner-ratio", size.width / size.height);
    element.style.setProperty("--banner-url", 'url(' + JSON.stringify(url) + ')');
    element.style.setProperty("--banner-width", size.width + "px");
    element.style.setProperty("--banner-height", size.height + "px");
  };
  if (!isBanner) { apply(null); return; }
  if (bannerSizeKnown.has(url)) { apply(bannerSizeKnown.get(url)); return; }
  if (!bannerSizeCache.has(url)) bannerSizeCache.set(url, new Promise(resolve => {
    const img = new Image();
    img.onload = () => resolve({ width: img.naturalWidth, height: img.naturalHeight });
    img.onerror = () => resolve(null);
    img.src = url;
  }));
  bannerSizeCache.get(url).then(size => {
    if (size) bannerSizeKnown.set(url, size);
    if (element.dataset.bannerUrl !== url) return;
    apply(size);
  });
}
function sampleCardArt(element, url) {
  const wash = element.classList.contains("icon-wash") || !!element.querySelector(".icon-wash");
  const color = element.dataset.artColor;
  element.classList.toggle("tinted-wash", wash && /^#[0-9a-f]{6}$/i.test(color || ""));
  element.style.setProperty("--art-tint", /^#[0-9a-f]{6}$/i.test(color || "") ? color : "transparent");
  element.classList.toggle("screenshot-art", /[?&]texture=1(?:&|$)/.test(url || ""));
  sizeCardBanner(element, url);
  if (typeof Image !== "function") return;
  if (!surfaceArtCache.has(url)) surfaceArtCache.set(url, new Promise(resolve => {
    const img = new Image(); img.crossOrigin = "anonymous";
    img.onload = () => { try {
      const c = document.createElement("canvas"); c.width=48; c.height=24;
      const ctx=c.getContext("2d", {willReadFrequently:true}); ctx.drawImage(img,0,0,48,24);
      resolve(artTreatment(ctx.getImageData(0,0,48,24).data));
    } catch { resolve(null); } };
    img.onerror=()=>resolve(null); img.src=url;
  }));
  element.dataset.artUrl=url;
  surfaceArtCache.get(url).then(value => {
    if (element.dataset.artUrl===url) element.style.setProperty("--art-card-brightness", String(value?.cardBrightness ?? .5));
  });
}
