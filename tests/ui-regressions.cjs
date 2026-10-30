const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

const source = (name) => fs.readFileSync(path.join(__dirname, "..", "ui", name), "utf8").replace(/\r\n/g, "\n");

function dialogContext() {
  const listeners = new Map();
  const timers = [];
  const intervals = new Map();
  let intervalId = 0;
  const document = {
    activeElement: null,
    createElement: (tag) => new Element(tag),
    addEventListener(type, fn, capture = false) { if (!listeners.has(type)) listeners.set(type, []); listeners.get(type).push({ fn, capture }); },
    removeEventListener(type, fn, capture = false) { listeners.set(type, listeners.get(type).filter((l) => l.fn !== fn || l.capture !== capture)); },
  };
  class Element {
    constructor(tag) { this.tagName = tag.toUpperCase(); this.children = []; this.events = {}; this.dataset = {}; this.style = {}; this.nodeType = 1; }
    append(...children) { for (const child of children) { child.parent = this; this.children.push(child); } }
    prepend(...children) { for (const child of children) child.parent = this; this.children.unshift(...children); }
    remove() { this.parent.children.splice(this.parent.children.indexOf(this), 1); this.parent = null; }
    contains(element) { return element === this || this.children.some((c) => c.contains(element)); }
    get isConnected() { return this === document.body || !!this.parent?.isConnected; }
    addEventListener(type, fn) { this.events[type] = fn; }
    setAttribute(key, value) { this[key] = value; }
    get tabIndex() { return this.tabindex == null ? (["BUTTON", "INPUT", "SELECT", "TEXTAREA", "A"].includes(this.tagName) ? 0 : -1) : Number(this.tabindex); }
    getClientRects() { return this.hidden ? [] : [{}]; }
    focus() { document.activeElement = this; }
    click() { return this.events.click?.(); }
    querySelector(selector) {
      const matches = (c) => selector.split(", ").some((s) => s === "button.primary" ? c.tagName === "BUTTON" && c.className.includes("primary") : s === "[tabindex]" ? c.tabindex != null : c.tagName === s.toUpperCase());
      for (const child of this.children) { if (matches(child)) return child; const found = child.querySelector(selector); if (found) return found; }
      return null;
    }
    querySelectorAll(selector) {
      const found = [];
      const visit = (node) => { for (const child of node.children) {
        if (selector.split(", ").some((s) => s === "[tabindex]" ? child.tabindex != null : child.tagName === s.toUpperCase())) found.push(child);
        visit(child);
      } };
      visit(this);
      return found;
    }
  }
  document.body = new Element("body");
  const context = vm.createContext({ document, window: { __TAURI__: {}, addEventListener() {} }, setTimeout: (fn) => timers.push(fn),
    setInterval: fn => { intervals.set(++intervalId, fn); return intervalId; }, clearInterval: id => intervals.delete(id) });
  vm.runInContext(source("common.js"), context);
  return {
    document, context, Element, intervals,
    flushFocus() { timers.splice(0).forEach((fn) => fn()); },
    key(key, target = document.activeElement, shiftKey = false) {
      const e = { key, target, shiftKey, ctrlKey: false, defaultPrevented: false, cancelBubble: false,
        preventDefault() { this.prevented = this.defaultPrevented = true; },
        stopPropagation() { this.cancelBubble = true; },
        stopImmediatePropagation() { this.stopped = this.cancelBubble = true; } };
      const handlers = [...listeners.get("keydown") || []];
      for (const { fn, capture } of handlers) { if (capture) fn(e); if (e.cancelBubble) return e; }
      for (let node = target; node; node = node.parent) { node.events.keydown?.(e); if (e.cancelBubble) return e; }
      for (const { fn, capture } of handlers) { if (!capture) fn(e); if (e.stopped) break; }
      return e;
    },
  };
}

test("only the top dialog handles keys and closing restores focus", async () => {
  const ui = dialogContext();
  const before = new ui.Element("input");
  ui.document.body.append(before);
  before.focus();
  const calls = [];
  ui.context.calls = calls;
  vm.runInContext('globalThis.parentDialog = modal({ title: "Parent", buttons: [{ label: "Save", primary: true, click: () => calls.push("parent") }] });', ui.context);
  ui.flushFocus();
  const parentFocus = ui.document.activeElement;
  vm.runInContext('globalThis.childDialog = modal({ title: "Child", buttons: [{ label: "OK", primary: true, click: () => calls.push("child") }] });', ui.context);
  // Enter before the deferred focus must not activate the parent's focused button.
  assert.equal(ui.key("Enter").prevented, true);
  await Promise.resolve();
  assert.deepEqual(calls, ["child"]);
  assert.equal(ui.document.activeElement, parentFocus);
  ui.flushFocus();
  ui.key("Escape");
  assert.equal(ui.document.activeElement, before);
  assert.deepEqual(calls, ["child"]);
});

test("Enter uses the primary button even when scheduler buttons were prepended", async () => {
  const ui = dialogContext();
  const calls = [];
  ui.context.calls = calls;
  vm.runInContext(`
    const body = el("input");
    const dialog = modal({ title: "Schedule", body, buttons: [{ label: "Cancel" }, { label: "Save", primary: true, click: () => calls.push("save") }] });
    dialog.box.querySelector("footer").prepend(el("button", { text: "Start now", onclick: () => calls.push("start") }), el("button", { text: "Stop now" }));
    body.focus();
  `, ui.context);
  ui.key("Enter");
  await Promise.resolve();
  assert.deepEqual(calls, ["save"]);
});

test("Escape closes only the topmost modal", () => {
  const ui = dialogContext();
  vm.runInContext('modal({ title: "Parent" }); modal({ title: "Child" });', ui.context);
  ui.flushFocus();
  ui.key("Escape", ui.document.body);
  assert.equal(ui.document.body.children.length, 1);
  ui.key("Escape", ui.document.body);
  assert.equal(ui.document.body.children.length, 0);
});

test("dialog controls handle their own Enter and Escape before modal shortcuts", async () => {
  for (const handling of ["preventDefault", "stopPropagation"]) {
    const ui = dialogContext();
    const calls = [];
    ui.context.calls = calls;
    vm.runInContext(`
      const control = el("input", { onkeydown: (e) => {
        if (e.key === "Enter" || e.key === "Escape") { e.${handling}(); calls.push(e.key); }
      } });
      modal({ title: "Limits", body: control, buttons: [{ label: "Save", primary: true, click: () => calls.push("save") }] });
      control.focus();
    `, ui.context);
    ui.key("Enter");
    ui.key("Escape");
    await Promise.resolve();
    assert.deepEqual(calls, ["Enter", "Escape"]);
    assert.equal(ui.document.body.children.length, 1);
    ui.key("Escape", ui.document.body);
    assert.equal(ui.document.body.children.length, 0);
  }
});

test("Tab wraps within the top dialog and skips hidden or disabled controls", () => {
  const ui = dialogContext();
  vm.runInContext(`
    globalThis.parentDialog = modal({ title: "Parent", buttons: [{ label: "Save", primary: true }] });
    const body = el("div", {}, el("input"), el("button", { disabled: true }), el("input", { hidden: true }));
    globalThis.childDialog = modal({ title: "Child", body, buttons: [{ label: "Save", primary: true }] });
  `, ui.context);
  ui.flushFocus();
  const child = ui.context.childDialog;
  const first = child.box.querySelector("button");
  const input = child.box.querySelector("input");
  const last = ui.document.activeElement;
  assert.equal(ui.key("Tab").prevented, true);
  assert.equal(ui.document.activeElement, first);
  assert.equal(ui.key("Tab", first, true).prevented, true);
  assert.equal(ui.document.activeElement, last);
  input.focus();
  assert.equal(ui.key("Tab").prevented, undefined);
  ui.document.body.focus();
  ui.key("Tab");
  assert.equal(ui.document.activeElement, first);
  assert.equal(child.box.role, "dialog");
  assert.equal(child.box["aria-modal"], "true");
  assert.equal(child.box["aria-labelledby"], child.box.querySelector("span").id);
  first.click();
  assert.equal(ui.document.body.children.length, 1);
});

test("backdrop dismisses plain dialogs but protects forms and the underlying modal", () => {
  for (const tag of [null, "input", "textarea", "select", "form"]) {
    const ui = dialogContext();
    ui.context.tag = tag;
    vm.runInContext('globalThis.dialog = modal({ title: "Dialog", body: tag ? el(tag) : null });', ui.context);
    const back = ui.context.dialog.back;
    back.events.mousedown({ target: back });
    back.events.mouseup({ target: back });
    assert.equal(back.isConnected, !!tag);
  }
  const ui = dialogContext();
  vm.runInContext('globalThis.parentDialog = modal({ title: "Parent" }); globalThis.childDialog = modal({ title: "Child", backdrop: false });', ui.context);
  for (const d of [ui.context.parentDialog, ui.context.childDialog]) {
    d.back.events.mousedown({ target: d.back });
    d.back.events.mouseup({ target: d.back });
    assert.equal(d.back.isConnected, true);
  }
});

test("release notes format headings and lists while keeping markup literal", () => {
  const ui = dialogContext();
  const main = source("main.js");
  const start = main.indexOf("function releaseNotes(");
  vm.runInContext(main.slice(start, main.indexOf("async function showUpdate", start)), ui.context);
  const notes = ui.context.releaseNotes("## Changes\n\n- <img src=x onerror=alert(1)>\n- Second\n\nA paragraph\ncontinued.");
  assert.deepEqual(notes.children.map((c) => c.tagName), ["H4", "UL", "P"]);
  assert.equal(notes.children[1].children[0].textContent, "<img src=x onerror=alert(1)>");
  assert.equal(notes.children[2].textContent, "A paragraph continued.");
});

test("shared theme events keep every desktop window dark", () => {
  for (const name of ["main", "add", "toast"]) {
    const handlers = new Map();
    const document = { documentElement: { dataset: {} }, addEventListener() {} };
    const context = vm.createContext({ document, window: { name, addEventListener() {}, __TAURI__: {
      event: { listen: async (event, callback) => handlers.set(event, callback) },
    } } });
    vm.runInContext(source("common.js"), context);
    for (const theme of ["Light", "Dark", "System"]) {
      handlers.get("theme-changed")({ payload: theme });
      assert.equal(document.documentElement.dataset.theme, "dark");
    }
  }
});

test("appearance settings reach open desktop windows with safe defaults", () => {
  for (const name of ["main", "add", "toast"]) {
    const handlers = new Map(), properties = new Map();
    const root = { dataset: {}, style: { setProperty: (k, v) => properties.set(k, v) } };
    const context = vm.createContext({ document: { documentElement: root, addEventListener() {} }, window: { name, addEventListener() {}, __TAURI__: {
      event: { listen: async (event, callback) => handlers.set(event, callback) },
    } } });
    vm.runInContext(source("common.js"), context);
    const apply = (settings) => handlers.get("appearance-changed")({ payload: settings });
    apply({ BackgroundColor: "forest", AccentColor: "#4cc2ff", Glass: "solid", LineGlow: 25, AccentFromArt: false, ArtBehindWindow: false, ArtSelectedOnly: false });
    assert.equal(properties.get("--bg"), "#0b1a14");
    assert.equal(properties.get("--accent"), "#4cc2ff");
    assert.equal(properties.get("--line-glow"), "0.25");
    assert.equal(root.dataset.glass, "solid");
    assert.equal(root.dataset.artBehindWindow, "false");
    assert.equal(root.dataset.artSelectedOnly, "false");
    assert.equal(root.dataset.accentFromArt, "false");
    apply({ BackgroundColor: "invalid", AccentColor: "url(invalid)", Glass: "invalid", LineGlow: 200 });
    assert.equal(properties.get("--bg"), "#0b1222");
    assert.equal(properties.get("--accent"), "#e9c98f");
    assert.equal(properties.get("--line-glow"), "1");
    assert.equal(root.dataset.glass, "ghost");
    assert.equal(root.dataset.artBehindWindow, "true");
    assert.match(properties.get("--select-arrow"), /data:image\/svg\+xml/);
  }
});

test("desktop icons use one accessible 24px outline set with a toothed settings gear", () => {
  const { context } = dialogContext();
  const svg = vm.runInContext('icon("gear")', context);
  assert.match(svg, /viewBox="0 0 24 24"/);
  assert.match(svg, /stroke-width="2"/);
  assert.match(svg, /stroke-linecap="round"/);
  assert.match(svg, /aria-hidden="true"/);
  assert.match(svg, /<path d="M[^"z]+L[^"z]+z"/);
  assert.match(svg, /z"\/><circle/);
  assert.match(svg, /<circle cx="12" cy="12" r="3"/);
});

test("a stalled renderer has at most one pending animation frame", async () => {
  let tick;
  let pending;
  let frames = 0;
  let beats = 0;
  const document = { hidden: false };
  const context = vm.createContext({ document, window: { __TAURI__: { core: { invoke: async () => beats++ } } },
    setInterval: (fn) => { tick = fn; }, requestAnimationFrame: (fn) => { frames++; pending = fn; } });
  vm.runInContext(source("guard.js"), context);
  for (let i = 0; i < 100; i++) tick();
  assert.equal(frames, 1);
  pending();
  await Promise.resolve();
  assert.equal(beats, 1);
  tick();
  assert.equal(frames, 2);
  document.hidden = true;
  pending();
  tick();
  assert.equal(frames, 2);
});

test("Check now sends the repository without saving the settings form", async () => {
  const calls = [];
  const context = vm.createContext({
    el: (_, attrs) => attrs,
    f: { UpdateRepo: { value: "  owner/repo  " } },
    updStatus: {},
    invoke: async (command, args) => { calls.push({ command, args }); return null; },
  });
  const main = source("main.js");
  const start = main.indexOf('  const updBtn = el(');
  const end = main.indexOf('\n\n  const appearanceKeys =', start);
  vm.runInContext(main.slice(start, end).replace("const updBtn", "globalThis.updBtn"), context);
  await context.updBtn.onclick();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, "check_update");
  assert.equal(calls[0].args.repo, "owner/repo");
});

test("countdown confirmation and expiry send their captured force choice", () => {
  for (const expire of [false, true]) {
    const calls = [];
    let dialog;
    let tick;
    const context = vm.createContext({
      listen: async () => () => {},
      el: () => ({}),
      modal: (options) => { dialog = options; return { close: (value) => options.onClose(value) }; },
      setInterval: (fn) => { tick = fn; return 1; }, clearInterval() {},
      invoke: async (command, args) => calls.push({ command, args }),
    });
    const main = source("main.js");
    const start = main.indexOf("function showCountdown(");
    vm.runInContext(main.slice(start, main.indexOf("\nasync function offerFirewall", start)), context);
    context.showCountdown({ action: "Shutdown", queue: "Downloads", force: false });
    if (expire) for (let i = 0; i < 60; i++) tick();
    else dialog.onClose("now");
    const actions=calls.filter(c=>c.command==="run_post_action");
    assert.equal(actions.length,1);
    calls.splice(0,calls.length,...actions);
    assert.equal(calls[0].command, "run_post_action");
    assert.deepEqual(Object.keys(calls[0].args).sort(), ["action", "force"]);
    assert.equal(calls[0].args.action, "Shutdown");
    assert.equal(calls[0].args.force, false);
  }
});

test("inactive details stop polling despite changes to relative activity text", async () => {
  let calls = 0, draws = 0;
  const elements = new Map();
  const row = { id: "torrent", status: "Paused", downloaded: 10, total: 100, path: "file" };
  const S = { cur: row.id, rows: new Map([[row.id, row]]), details: null, tab: "general" };
  const context = vm.createContext({ S, performance: { now: () => 1000 }, renderDetailHeader() {}, renderConnectionTable() {}, document: { hidden: false },
    $: (key) => { if (!elements.has(key)) elements.set(key, {}); return elements.get(key); },
    invoke: async () => { calls++; return { segInfo: "paused" }; },
    drawSeg: () => draws++, drawGraph: () => draws++, renderTorrentTab() {},
  });
  const main = source("main.js");
  const start = main.indexOf("let detailsBusy = false;");
  vm.runInContext(main.slice(start, main.indexOf("\nfunction css(", start)), context);
  await context.refreshDetails();
  assert.equal(calls, 1);
  for (let i = 0; i < 10; i++) {
    S.rows.set(row.id, { ...row, lastActivity: `${i} seconds ago` });
    await context.refreshDetails(true);
  }
  assert.equal(calls, 1);
  assert.equal(draws, 2);
  S.rows.set(row.id, { ...row, downloaded: 20 });
  await context.refreshDetails(true);
  assert.equal(calls, 2);
  await context.refreshDetails(); // Explicit changes to file selection/priority still refresh.
  assert.equal(calls, 3);
  for (const activity of [{ running: true }, { status: "Completed", seeding: true }]) {
    S.rows.set(row.id, { ...row, ...activity });
    await context.refreshDetails(true);
    await context.refreshDetails(true);
  }
  assert.equal(calls, 7);
  S.rows.set(row.id, { ...row, status: "Completed", seeding: false });
  await context.refreshDetails(true);
  await context.refreshDetails(true);
  assert.equal(calls, 8);
});

test("explicit details refreshes survive an in-flight request and selection changes", async () => {
  for (const changeSelection of [false, true]) {
    const pending = [], elements = new Map();
    const row = { id: "first", status: "Paused" };
    const S = { cur: row.id, rows: new Map([[row.id, row], ["second", { ...row, id: "second" }]]), details: null, tab: "general" };
    const context = vm.createContext({ S, performance: { now: () => 1000 }, renderDetailHeader() {}, renderConnectionTable() {}, document: { hidden: false },
      $: (key) => { if (!elements.has(key)) elements.set(key, {}); return elements.get(key); },
      invoke: (_, args) => new Promise(resolve => pending.push({ id: args.id, resolve })),
      drawSeg() {}, drawGraph() {}, renderTorrentTab() {},
    });
    const main = source("main.js");
    const start = main.indexOf("let detailsBusy = false;");
    vm.runInContext(main.slice(start, main.indexOf("\nfunction css(", start)), context);
    const first = context.refreshDetails();
    if (changeSelection) S.cur = "second";
    await context.refreshDetails(); // A selection or content edit must fetch again after the old response.
    await context.refreshDetails(true); // Periodic polling must not erase the explicit refresh.
    assert.equal(pending.length, 1);
    pending[0].resolve({ segInfo: "old" });
    await first;
    assert.equal(pending.length, 2);
    assert.equal(pending[1].id, S.cur);
    pending[1].resolve({ segInfo: "updated" });
    await Promise.resolve();
    await context.refreshDetails(true);
    assert.equal(S.details.segInfo, "updated");
    assert.equal(pending.length, 2);
  }
});

test("the footer shows seeding activity when no downloads are running", () => {
  const elements = new Map();
  const S = { summary: { running: 0, count: 3, seeding: 2, upSpeedText: "45 KB/s" } };
  const context = vm.createContext({ S, document: {}, fmtSize: String,
    $: (key) => { if (!elements.has(key)) elements.set(key, {}); return elements.get(key); },
  });
  const main = source("main.js");
  const start = main.indexOf("function renderStatus() {");
  vm.runInContext(main.slice(start, main.indexOf("\nlet globalCombo", start)), context);
  context.renderStatus();
  assert.equal(elements.get("#statusLeft").textContent, "Seeding 2 · ↑ 45 KB/s");
  S.summary.running = 1;
  S.summary.speedText = "100 KB/s";
  context.renderStatus();
  assert.match(elements.get("#statusLeft").textContent, /1 running.*Seeding 2.*45 KB\/s/);
});

function extensionContext(gecko, storage = {}) {
  const event = () => ({ listeners: [], addListener(fn) { this.listeners.push(fn); } });
  const changes = { paused: [], cancelled: [], sent: [] };
  const area = {
    async get(key) { return { [key]: structuredClone(storage[key]) }; },
    async set(value) { Object.assign(storage, structuredClone(value)); },
    async remove(key) { delete storage[key]; },
  };
  const ext = {
    runtime: { id: "test" }, storage: { local: area, session: area },
    downloads: { onCreated: event(), pause: async (id) => changes.paused.push(id),
      cancel: async (id) => changes.cancelled.push(id), erase: async () => {} },
    webRequest: Object.fromEntries(["onBeforeRequest", "onBeforeRedirect", "onCompleted", "onErrorOccurred", "onHeadersReceived"].map(k => [k, event()])),
  };
  if (!gecko) ext.downloads.onDeterminingFilename = event();
  const context = vm.createContext({ ext, isGecko: gecko, URL, setTimeout() {}, flash() {},
    loadSettings: async () => ({ enabled: true, allTypes: true }), extSet: () => new Set(), extOf: () => "zip",
    send: async (urls) => changes.sent.push(...urls),
  });
  let background = fs.readFileSync(path.join(__dirname, "..", "src-tauri", "extension", "background.js"), "utf8").replace(/\r\n/g, "\n");
  background = background.slice(background.indexOf("\n") + 1, background.indexOf("// ---------------------------------------------------------------- sending to FluxDM"));
  vm.runInContext(background, context);
  return { context, changes, ext, storage,
    request(d) { for (const fn of ext.webRequest.onBeforeRequest.listeners) fn(d); },
    redirect(d) { for (const fn of ext.webRequest.onBeforeRedirect.listeners) fn(d); },
  };
}

test("POST downloads and their redirect chains stay in both browsers", async () => {
  for (const gecko of [false, true]) {
    const browser = extensionContext(gecko);
    const original = "https://example.invalid/export";
    const redirect = "https://example.invalid/ready";
    const final = "https://cdn.invalid/file.zip";
    browser.request({ method: "POST", url: original, requestId: "post" });
    browser.redirect({ method: "POST", url: original, redirectUrl: redirect, requestId: "post" });
    browser.request({ method: "GET", url: redirect, requestId: "post" });
    browser.redirect({ method: "GET", url: redirect, redirectUrl: final, requestId: "post" });
    browser.request({ method: "GET", url: final, requestId: "post" });
    const item = { id: 1, url: gecko ? final : original, finalUrl: final, filename: "file.zip", state: "in_progress" };
    await browser.context.capture(item);
    await browser.context.onHeaders({ method: "GET", url: final, statusCode: 200, responseHeaders: [{ name: "content-disposition", value: "attachment; filename=file.zip" }] });
    assert.deepEqual(browser.changes, { paused: [], cancelled: [], sent: [] });
    // The service worker can be suspended between the response and the downloads event.
    const resumed = extensionContext(gecko, browser.storage);
    await resumed.context.capture(item);
    const next = "https://cdn.invalid/after-suspension.zip";
    resumed.redirect({ method: "GET", url: final, redirectUrl: next, requestId: "post" });
    await resumed.context.capture({ ...item, url: next, finalUrl: next });
    assert.deepEqual(resumed.changes, { paused: [], cancelled: [], sent: [] });
    const getUrl = "https://example.invalid/normal.zip";
    resumed.request({ method: "GET", url: getUrl, requestId: "get" });
    await resumed.context.capture({ ...item, id: 2, url: getUrl, finalUrl: getUrl });
    assert.deepEqual(resumed.changes, { paused: [2], cancelled: [2], sent: [getUrl] });
  }
});
function listContext() {
  class Node {
    constructor(attrs = {}) { Object.assign(this, attrs); this.children = []; this.classList = { toggle() {} }; }
    get firstChild() { return this.children[0] || null; }
    get nextSibling() { return this.parent?.children[this.parent.children.indexOf(this) + 1] || null; }
    insertBefore(node, before) { node.remove(); const i = before ? this.children.indexOf(before) : this.children.length; this.children.splice(i, 0, node); node.parent = this; }
    remove() { if (this.parent) this.parent.children.splice(this.parent.children.indexOf(this), 1); this.parent = null; }
  }
  const elements = new Map(), $ = (key) => { if (!elements.has(key)) elements.set(key, new Node()); return elements.get(key); };
  const S = { rows: new Map(), order: [], sortKey: "Added", sortDir: -1, groupBy: "None", cols: [], search: "", nav: "all", sel: new Set() }, trs = new Map();
  const context = vm.createContext({ S, trs, $, URL, $$: (_, root) => root.children.filter((e) => e.class === "day-group"),
    el: (_, attrs, child) => { const node = new Node(attrs); if (child) node.insertBefore(child, null); return node; } });
  const main = source("main.js");
  vm.runInContext(main.slice(main.indexOf("const COLUMNS"), main.indexOf("const S =")) +
    main.slice(main.indexOf("function visibleCols"), main.indexOf("function statusClass")) +
    main.slice(main.indexOf("function matchesNav"), main.indexOf("// ---------------------------------------------------------------- columns & layout")) +
    main.slice(main.indexOf("function dayLabel"), main.indexOf("function cardHtml")), context);
  function add(row) { S.rows.set(row.id, { name: row.id, url: "", tags: [], ...row }); S.order.push(row.id); trs.set(row.id, new Node({ id: row.id })); }
  return { context, S, trs, add, elements, $ };
}

test("download sorting defaults newest first, handles every sort field and keeps ties stable", () => {
  const ui = listContext();
  ui.add({ id: "a", name: "Beta", addedTs: 100, total: 300, progress: 90, speed: 20, upSpeed: 10, eta: "1m", downloaded: 100, status: "Paused", category: "Video" });
  ui.add({ id: "b", name: "Alpha", addedTs: 200, total: 200, progress: 10, speed: 30, upSpeed: 50, eta: "1m", downloaded: 100, status: "Downloading", category: "Programs" });
  ui.add({ id: "c", name: "Gamma", addedTs: 100, total: 100, progress: 50, speed: 10, upSpeed: 20, eta: "", downloaded: 0, status: "Queued", category: "Torrents" });
  const ids = () => Array.from(ui.context.sortedIds());
  assert.deepEqual(ids(), ["b", "a", "c"]);
  const cases = { "File name": ["b", "a", "c"], Size: ["c", "b", "a"], Progress: ["b", "c", "a"], Speed: ["c", "a", "b"], "Up Speed": ["a", "c", "b"], "Time left": ["b", "a", "c"], Status: ["b", "a", "c"], Category: ["b", "c", "a"] };
  for (const [key, expected] of Object.entries(cases)) { ui.S.sortKey = key; ui.S.sortDir = 1; assert.deepEqual(ids(), expected, key); ui.S.sortDir = -1; assert.deepEqual(ids(), [...expected].reverse(), key); }
});

test("day grouping follows local dates, filtering and sorting without stale headers", () => {
  const ui = listContext(), today = new Date(); today.setHours(12, 0, 0, 0);
  const yesterday = new Date(today); yesterday.setDate(yesterday.getDate() - 1);
  const old = new Date(today); old.setDate(old.getDate() - 4);
  ui.add({ id: "today", addedTs: today.getTime() / 1000 }); ui.add({ id: "yesterday", addedTs: yesterday.getTime() / 1000 }); ui.add({ id: "old", addedTs: old.getTime() / 1000 });
  const groups = () => ui.$("#rows").children.filter((e) => e.class === "day-group").map((e) => e.children[0].text);
  ui.context.arrange(); assert.deepEqual(groups(), []);
  ui.S.groupBy = "Date added";
  ui.context.arrange(); assert.deepEqual(groups().slice(0, 2), ["TODAY", "YESTERDAY"]); assert.equal(groups().length, 3);
  ui.context.arrange(); assert.equal(groups().length, 3);
  ui.S.search = "yesterday"; ui.context.arrange(); assert.deepEqual(groups(), ["YESTERDAY"]); assert.equal(ui.$("#listCount").textContent, "1 downloads");
  ui.S.search = ""; ui.S.sortDir = 1; ui.context.arrange(); assert.equal(groups()[0], "TODAY");
  ui.S.sortKey = "File name"; ui.context.arrange(); assert.equal(groups().length, 3);
  ui.S.groupBy = "None"; ui.context.arrange(); assert.deepEqual(groups(), []);
  assert.equal(ui.context.dayLabel(0), "UNKNOWN DATE");
  const newYear = new Date(2026, 0, 1, 12); assert.equal(ui.context.dayLabel(new Date(2025, 11, 31, 12).getTime() / 1000, newYear), "YESTERDAY");
});

test("layout persistence saves sort, direction and list width without replacing settings", () => {
  let args;
  const S = { sortKey: "Speed", sortDir: 1, groupBy: "Queue", tableView: false, cols: [], settings: {} };
  const context = vm.createContext({ S, clearTimeout() {}, setTimeout: (fn) => fn(),
    $: () => ({ style: {}, getBoundingClientRect: () => ({ width: 360 }) }), invoke: (command, value) => { assert.equal(command, "save_layout"); args = value; } });
  const main = source("main.js"); vm.runInContext(main.slice(main.indexOf("let layoutTimer"), main.indexOf("function updateSortControls")), context);
  context.saveLayout(); assert.equal(args.layout.groupBy, "Queue"); assert.equal(S.settings.DownloadGroupBy, "Queue"); assert.equal(args.layout.sortKey, "Speed"); assert.equal(args.layout.sortDirection, 1); assert.equal(args.layout.listWidth, 360);
});

test("art accent ignores dark pixels and produces a readable dominant light colour", () => {
  const context = vm.createContext({}); const main = source("common.js");
  vm.runInContext(main.slice(main.indexOf("function artAccent"),main.indexOf("const surfaceArtCache")), context);
  assert.equal(context.artAccent(new Uint8ClampedArray([2, 3, 4, 255])), null);
  const pixels = new Uint8ClampedArray([190, 130, 60, 255, 195, 135, 65, 255, 30, 100, 200, 255]);
  const accent = context.artAccent(pixels); assert.match(accent, /^#[a-f0-9]{6}$/);
  const rgb = accent.slice(1).match(/../g).map((v) => parseInt(v, 16)); assert.ok(.2126 * rgb[0] + .7152 * rgb[1] + .0722 * rgb[2] >= 169);
});

test("inactive cards describe state without download speed or connection counts", () => {
  const main = source("main.js");
  const context = vm.createContext({ S: { cur: "", settings: {} }, CATEGORY_ICON: {},
    esc: value => String(value ?? "").replaceAll("<", "&lt;"), icon: () => "", fallbackArt: () => "", fmtSpeed: v => v + " B/s", fmtSize: () => "1 MB", visibleCols: () => [] });
  vm.runInContext(main.slice(main.indexOf("function statusLabel("), main.indexOf("function cellHtml(")) + main.slice(main.indexOf("function rowActivity"), main.indexOf("function renderDetailHeader")) + main.slice(main.indexOf("function cardHtml"), main.indexOf("let artKey")), context);
  const row = { id: "test", name: "File", category: "Other", progress: 62, downloaded: 62, total: 100, sizeText: "1 MB", conn: "8/8", speedText: "0 B/s", upSpeedText: "20 KB/s", ratio: "0.5", completed: "Oct 29 13:00", queue: "Night queue", error: "<disk full>\nLong diagnostic" };
  for (const [status, expected] of [["Paused", "Paused at 62%"], ["Queued", "Night queue"], ["Completed", "Done \u00b7 Oct 29 13:00"], ["Error", "&lt;disk full>"]]) {
    const html = context.cardHtml({ ...row, status });
    assert.ok(html.includes(expected), status);
    assert.ok(!html.includes("0 B/s"), status);
    assert.ok(!html.includes("8/8"), status);
  }
  // Selecting a row must not change its HTML (a rebuild reloads its pictures and makes the list blink).
  context.S.cur = "test";
  const selectedHtml = context.cardHtml({ ...row, status: "Paused" });
  context.S.cur = "";
  assert.equal(selectedHtml, context.cardHtml({ ...row, status: "Paused" }));
  const sharing = context.cardHtml({ ...row, status: "Completed", torrent: true, seeding: true });
  assert.ok(sharing.includes("Sharing \u2191 20 KB/s \u00b7 ratio 0.5"));
  assert.ok(!sharing.includes("0 B/s"));
  assert.ok(context.cardHtml({ ...row, status: "Downloading" }).includes("8/8"));
});

test("details tabs preserve shared controls when switching transfer types", () => {
  const main = source("main.js"), tabs = ["general", "options", "mirrors", "log", "peers", "trackers", "content", "pieces"];
  const pages = tabs.map(t => ({ id: "tab-" + t }));
  const buttons = tabs.map(t => ({ dataset: { tab: t }, classList: { toggle() {} }, setAttribute() {} }));
  const S = { cur: "item", rows: new Map([["item", { torrent: false }]]) };
  const context = vm.createContext({ S, $$: key => key === ".tab" ? buttons : pages, $: () => ({ classList: { toggle() {} } }), refreshDetails() {}, drawSeg() {}, drawGraph() {} });
  vm.runInContext(main.slice(main.indexOf("function setTab"), main.indexOf("function renderDetails")), context);
  for (const t of ["general", "options", "mirrors", "log"]) { context.setTab(t); assert.equal(S.tab, t); assert.equal(pages.filter(p => !p.hidden)[0].id, "tab-" + t); }
  context.setTab("peers"); assert.equal(S.tab, "general");
  S.rows.get("item").torrent = true;
  for (const t of ["peers", "trackers", "content", "pieces", "options", "log"]) { context.setTab(t); assert.equal(S.tab, t); }
  context.setTab("mirrors"); assert.equal(S.tab, "peers");
});

test("bright artwork stays near the base luminance and accents stay muted", () => {
  const context = vm.createContext({}); const main = source("common.js");
  vm.runInContext(main.slice(main.indexOf("function artAccent"), main.indexOf("const surfaceArtCache")), context);
  for (const colour of [[255,240,0],[255,255,255],[20,25,40],[255,0,255]]) {
    const treatment = context.artTreatment(new Uint8ClampedArray([...colour,255]));
    const luminance = .2126*colour[0]+.7152*colour[1]+.0722*colour[2];
    assert.ok(luminance*treatment.brightness <= 9.01);
    assert.ok(luminance*treatment.cardBrightness <= 110.01);
    assert.ok(treatment.cardBrightness > treatment.brightness);
    assert.ok(treatment.blur >= 34);
    if (treatment.accent) {
      const rgb=treatment.accent.slice(1).match(/../g).map(v=>parseInt(v,16)/255);
      const max=Math.max(...rgb),min=Math.min(...rgb),light=(max+min)/2;
      assert.ok(light >= .695 && light <= .765);
      assert.ok((max-min)/(1-Math.abs(2*light-1)) <= .59);
    }
  }
});
test("completed rows omit unknown completion dates", () => {
  const context = vm.createContext({}); const main = source("main.js");
  vm.runInContext(main.slice(main.indexOf("function rowActivity"),main.indexOf("function renderDetailHeader")),context);
  assert.equal(context.rowActivity({status:"Completed"}),"Done");
  assert.equal(context.rowActivity({status:"Completed",completed:"Oct 29"}),"Done \u00b7 Oct 29");
});

test("modal close guards keep unsaved forms open until discard is accepted", async () => {
  const ui=dialogContext();let allow=false,closes=0;
  ui.context.guard=async()=>allow;ui.context.closed=()=>closes++;
  vm.runInContext('globalThis.dialog=modal({title:"Settings",beforeClose:guard,onClose:closed})',ui.context);
  await ui.context.dialog.close();assert.equal(closes,0);assert.ok(ui.context.dialog.back.isConnected);
  allow=true;await ui.context.dialog.close();assert.equal(closes,1);assert.ok(!ui.context.dialog.back.isConnected);
});
function toastContext() {
  const elements=new Map(),listeners=new Map(),timers=[],calls=[];
  const element=key=>{
    if(!elements.has(key)) elements.set(key,{dataset:{},querySelector:()=>null,style:{setProperty(){},removeProperty(){}},classList:{toggle(){},contains:()=>false,remove(){}},children:[],events:{},textContent:'',innerHTML:'',getBoundingClientRect:()=>({height:190}),replaceChildren(...children){this.children=children;this.textContent=children.map(c=>typeof c==='string'?c:c.text??'').join('');},addEventListener(ev,fn){this.events[ev]=fn;}});
    return elements.get(key);
  };
  let now=10000;
  const context=vm.createContext({
    $:element,icon:k=>k,esc:s=>s,applyAppearance(){},applySurfaceArt(){},
    el:(tag,attrs)=>attrs,
    invoke:async(cmd,args)=>{calls.push([cmd,args]);return cmd==='get_settings'?{settings:{}}:cmd==='toast_ready'?[]:null;},
    listen:async(ev,fn)=>{listeners.set(ev,fn);return ()=>{};},
    setInterval:fn=>timers.push(fn),Date:{now:()=>now},
  });
  const common=source('common.js');
  vm.runInContext(common.slice(common.indexOf('function fallbackArt')),context);
  vm.runInContext(source('toast.js'),context);
  return {context,element,calls,listeners,tick(ms){now+=ms;timers.forEach(fn=>fn());}};
}

test('toasts preserve arrival order, hover holds, reset recovery and explicit item actions',async()=>{
  const t=toastContext(),c=t.context;
  c.receive({gen:1,kind:'finished',id:'one',title:'One finished',size:'1 GB',checksum:true});
  c.receive({gen:2,kind:'error',id:'two',title:'Download failed',body:'Connection lost'});
  assert.equal(t.element('#title').textContent,'One finished');
  assert.match(t.element('#body').textContent,/checksum OK/);
  assert.equal(t.element('#actions').children[0].text,'Open');
  t.element('#t').events.mouseenter();t.tick(7000);
  assert.equal(t.element('#title').textContent,'One finished');
  t.element('#t').events.mouseleave();t.tick(6600); // resumes from the time left when the pointer entered
  assert.equal(t.element('#title').textContent,'Download failed');
  await t.element('#actions').children[0].onclick({stopPropagation(){}});
  assert.ok(t.calls.some(([cmd,args])=>cmd==='start'&&args.ids[0]==='two'));
  c.receive({gen:3,title:'Three'});c.receive({gen:4,title:'Four'});
  t.listeners.get('toast-reset')();assert.equal(t.element('#title').textContent,'Four');
  c.dismiss();assert.ok(t.calls.some(([cmd,args])=>cmd==='toast_hide'&&args.gen===4));
});

test('countdown toasts use the shared token, action names and closed-state synchronization',async()=>{
  const t=toastContext(),c=t.context;
  c.receive({gen:1,kind:'countdown',token:'queue-1',queue:'Main',action:'Hibernate',seconds:60});
  assert.equal(t.element('#title').textContent,'Main queue finished');
  assert.equal(t.element('#actions').children[1].text,'Hibernate now');
  t.listeners.get('countdown-state')({token:'queue-1',queue:'Main',action:'Hibernate',seconds:52});
  assert.match(t.element('#body').textContent,/52 s/);
  t.tick(20000);assert.match(t.element('#body').textContent,/52 s/);
  await t.element('#actions').children[1].onclick({stopPropagation(){}});
  assert.ok(t.calls.some(([cmd,args])=>cmd==='countdown_choice'&&args.token==='queue-1'&&args.now));
  c.receive({gen:2,title:'First'});c.receive({gen:3,kind:'countdown',token:'stale',seconds:60});
  t.listeners.get('countdown-state')({token:'stale',closed:true});
  t.tick(7000);
  assert.ok(t.calls.some(([cmd,args])=>cmd==='toast_hide'&&args.gen===2));
  c.receive({gen:4,title:'Next'});assert.equal(t.element('#title').textContent,'Next');
});

test('countdown titles handle missing and blank queue names; retry icons retain their error tile',()=>{
  const t=toastContext(),c=t.context;
  for(const queue of [undefined,'','   ']) {
    c.renderCountdown({queue,action:'Sleep',seconds:52});
    assert.equal(t.element('#title').textContent,'Queue finished');
  }
  c.renderCountdown({queue:' Main ',action:'Sleep',seconds:52});
  assert.equal(t.element('#title').textContent,'Main queue finished');
  for(const kind of ['error','retrying']) {
    c.show({kind,icon:'game-icon',banner:'game-art',title:'Connection lost'});
    assert.equal(t.element('#t').className,'error');
    assert.equal(t.element('#logo').innerHTML,kind==='retrying'?'wifiOff':'warn'); // only a retry is a connection problem
  }
});

test('yellow banner accents stay warm with the shared artwork clamp',()=>{
  const context=vm.createContext({}),common=source('common.js');
  vm.runInContext(common.slice(common.indexOf('function artAccent'),common.indexOf('const surfaceArtCache')),context);
  const accent=context.artTreatment(new Uint8ClampedArray([255,240,0,255])).accent;
  const [r,g,b]=accent.slice(1).match(/../g).map(v=>parseInt(v,16));
  assert.ok(r>g+10 && g>b+30,accent);
});

test('Add selection payload retains filtered choices, priorities and all request options',async()=>{
  const fields=new Map(),calls=[];
  const get=key=>{if(!fields.has(key))fields.set(key,{value:'',checked:false});return fields.get(key);};
  const files=[{index:0,on:true,length:100,priority:4},{index:1,on:false,length:50,priority:1}];
  const cls={urls:['magnet:?xt=urn:btih:test'],torrent:true};
  const A={nameEdited:false,folderEdited:true,torrent:{name:'Torrent',infoHash:'hash',cachedFile:'cache'},torrentFiles:files,probedUrl:cls.urls[0]};
  Object.entries({'#limit':'500k','#upLimit':'20k','#seedLimit':'0','#headers':'Cookie: session\nAuthorization: token','#queue':'Main','#folder':'Preview','#conn':'8','#mirrors':'https://example.invalid/mirror'}).forEach(([k,v])=>get(k).value=v);
  get('#openDone').checked=true;
  const c=vm.createContext({A,$:get,urlsBox:{value:cls.urls[0]},nameBox:{value:'Torrent'},folderBox:get('#folder'),parseSpeed:s=>parseInt(s)||0,videoMode:()=>false,
    invoke:async(cmd,args)=>{if(cmd==='classify')return cls;calls.push([cmd,args]);},alertBox(){throw Error('unexpected alert');}});
  const add=source('add.js');vm.runInContext(add.slice(add.indexOf('async function doSubmit'),add.indexOf('\nurlsBox.addEventListener',add.indexOf('async function doSubmit'))),c);
  await c.doSubmit(false);const req=calls[0][1].req;
  assert.equal(req.mode,'torrent');assert.equal(req.start,false);assert.equal(req.openWhenDone,true);
  assert.deepEqual(Array.from(req.skipped),[1]);assert.equal(req.filePriorities[0],4);assert.equal(req.wantedSize,100);
  assert.match(req.headers,/Cookie: session/);assert.equal(req.queue,'Main');assert.equal(req.uploadLimitKbps,20);
});


test("rail widths reveal labels and stay bounded at the minimum window width", () => {
  const sidebar={style:{},classList:{toggle(k,v){this[k]=v;}}};
  const c=vm.createContext({S:{},innerWidth:1440,$:()=>sidebar});
  const main=source('main.js');
  vm.runInContext(main.slice(main.indexOf('function setRailWidth'),main.indexOf('function loadLayout')),c);
  for(const [width,expected,label] of [[20,62,false],[149,149,false],[150,150,true],[900,240,true]]) {
    c.setRailWidth(width);assert.equal(sidebar.style.width,expected+'px');assert.equal(sidebar.classList.expanded,label);
  }
  c.innerWidth=640;c.setRailWidth(240);assert.equal(c.S.railWidth,150);
  c.setRailWidth(62);assert.equal(sidebar.classList.expanded,false);
});

test("cards use clean titles, retain filenames and replace watermark artwork with icon washes", () => {
  const c=vm.createContext({S:{cur:'1',settings:{},cols:[]},esc:s=>s,icon:k=>'<svg>'+k+'</svg>',fmtSize:()=> '1 MB',rowActivity:()=> 'Done',visibleCols:()=>[],CATEGORY_ICON:{Programs:'app'}});
  const common=source('common.js');vm.runInContext(common.slice(common.indexOf('function fallbackArt')),c);
  const main=source('main.js');vm.runInContext(main.slice(main.indexOf('function statusLabel('),main.indexOf('function cellHtml('))+main.slice(main.indexOf('function cardHtml'),main.indexOf('let artKey')),c);
  const html=c.cardHtml({id:'1',name:'Game_Name_setup.exe',title:'Game Name',category:'Programs',icon:'icon.png',status:'Completed'});
  assert.match(html,/>Game Name<\/strong>/);assert.match(html,/card-filename[^>]*[^]*>Game_Name_setup.exe<\/small>/);
  assert.match(html,/<img src="icon.png" class="icon-wash"/);
  assert.ok(c.fallbackArt('file').startsWith('data:image/svg+xml,'));
  assert.match(main,/#dToggle"\)\.hidden = !r\.torrent && r\.status === "Completed"/);
});

test("finished file selections hide Resume while torrent sharing and mixed selections remain available", () => {
  const button = {}, rows = [];
  const c = vm.createContext({ selected: () => rows, $: () => button });
  const main = source('main.js');
  vm.runInContext(main.slice(main.indexOf('function updateResumeAction'), main.indexOf('function onRowClick')), c);
  rows.push({status:'Completed',torrent:false});c.updateResumeAction();assert.equal(button.hidden,true);
  rows.push({status:'Paused',torrent:false});c.updateResumeAction();assert.equal(button.hidden,false);
  rows.splice(0,rows.length,{status:'Completed',torrent:true});c.updateResumeAction();
  assert.equal(button.hidden,false);assert.equal(button.title,'Resume sharing');
  rows.length=0;c.updateResumeAction();assert.equal(button.hidden,false);
});


test("grouping clusters status, category, site and queue while preserving the chosen sort inside each group", () => {
  const ui = listContext();
  ui.add({ id: "a", addedTs: 10, status: "Paused", category: "Video", queue: "Night", url: "https://one.invalid/a" });
  ui.add({ id: "b", addedTs: 30, status: "Completed", seeding: true, category: "Torrents", torrent: true, queue: "", url: "magnet:?xt=abc" });
  ui.add({ id: "c", addedTs: 20, status: "Paused", category: "Video", queue: "Night", url: "https://one.invalid/c" });
  ui.add({ id: "d", addedTs: 40, status: "Completed", category: "Programs", queue: "Day", url: "bad url" });
  for (const mode of ["Status", "Type/category", "Site", "Queue"]) {
    ui.S.groupBy = mode; ui.context.arrange();
    const ids = Array.from(ui.context.sortedIds());
    assert.ok(ids.indexOf("c") + 1 === ids.indexOf("a"), mode);
    const labels = ids.map(id => ui.context.groupLabel(ui.S.rows.get(id)));
    assert.deepEqual([...new Set(labels)], [...new Set(labels)].sort((a,b) => a.localeCompare(b)));
  }
  ui.S.groupBy = "Status"; assert.equal(ui.context.groupLabel(ui.S.rows.get("b")), "Seeding");
  ui.S.groupBy = "None"; assert.deepEqual(Array.from(ui.context.sortedIds()), ["d", "b", "c", "a"]);
});

test("small banners use intrinsic dimensions and stale image loads cannot change the current artwork", async () => {
  const pending = [];
  class Image { set src(url) { this.url = url; pending.push(this); } }
  const context = vm.createContext({ Image });
  const common = source("common.js");
  vm.runInContext(common.slice(common.indexOf("const bannerSizeCache"), common.indexOf("function sampleCardArt")), context);
  const classes = new Set(), props = {};
  const element = { dataset: {}, querySelector: () => null, classList: { contains: c => classes.has(c), remove: c => classes.delete(c), toggle: (c,on) => on ? classes.add(c) : classes.delete(c) }, style: { setProperty: (k,v) => props[k] = v } };
  context.sizeCardBanner(element, "header");
  Object.assign(pending[0], { naturalWidth: 460, naturalHeight: 215 }); pending[0].onload(); await Promise.resolve();
  assert.ok(classes.has("small-banner")); assert.equal(props["--banner-width"], "460px");
  context.sizeCardBanner(element, "capsule"); context.sizeCardBanner(element, "hero");
  Object.assign(pending[2], { naturalWidth: 1920, naturalHeight: 620 }); pending[2].onload(); await Promise.resolve();
  Object.assign(pending[1], { naturalWidth: 616, naturalHeight: 353 }); pending[1].onload(); await Promise.resolve();
  assert.ok(!classes.has("small-banner")); assert.equal(props["--banner-width"], "1920px");
  classes.add("icon-wash"); context.sizeCardBanner(element, "icon"); assert.equal(pending.length, 3);
});

test("torrent phase pickers patch only the current item and preserve No upload", () => {
  const calls = [], combos = [], row = { id: "one", torrent: true, uploadLimitKBps: -1, seedingLimitKBps: 128 };
  const S = { cur: "one", rows: new Map([["one",row]]) };
  const context = vm.createContext({ S, $: id => id, UPLOAD_PRESETS: [], speedCombo: (box,opts) => { combos.push({box,...opts}); return {}; }, invoke: (...args) => calls.push(args) });
  const main = source("main.js");
  vm.runInContext('let detailUpload, detailSeeding;'+main.slice(main.indexOf('  const phaseLimit ='), main.indexOf('  $("#dOpenDone").addEventListener')), context);
  assert.equal(combos[0].get(), -1); assert.equal(combos[1].get(), 128);
  combos[0].set(256); combos[1].set(-1);
  assert.equal(JSON.stringify(calls), JSON.stringify([["set_items",{ids:["one"],patch:{uploadLimitKBps:256}}],["set_items",{ids:["one"],patch:{seedingLimitKBps:-1}}]]));
  assert.ok(combos.every(c => c.allowNone));
});


test("saved grouping is independent of sorting and older layouts migrate to None", () => {
  const main = source("main.js"), S = { settings: {} };
  const context = vm.createContext({ S, COLUMNS: [{ k: "Added", sort: true }] });
  vm.runInContext(main.slice(main.indexOf('const GROUP_OPTIONS'), main.indexOf('function groupDate')) + main.slice(main.indexOf('function loadLayout'), main.indexOf('  S.tableView = st.DownloadTableView')) + '}', context);
  for (const [saved, expected] of [[undefined,"None"],["invalid","None"],["Date added","Date added"],["Queue","Queue"]]) {
    S.settings = { DownloadSortKey: "Added", DownloadGroupBy: saved };
    context.loadLayout(); assert.equal(S.groupBy, expected); assert.equal(S.sortKey, "Added");
  }
});

test("Bandwidth limit exposes upload phases only for an all-torrent selection", async () => {
  const main = source("main.js"), patches = []; let items;
  const S = { cur: "a", rows: new Map(), summary: {} };
  const context = vm.createContext({ S, selected: () => [...S.rows.values()], act: {}, LIMIT_PRESETS: [["Unlimited",0],["1 MB/s",1024]], UPLOAD_PRESETS: [["No upload",-1],["Unlimited",0]], fmtLimit: String, invoke: (...args) => patches.push(args), showMenu: value => items = value });
  vm.runInContext(main.slice(main.indexOf('async function limitMenuItems'), main.indexOf('// ---------------------------------------------------------------- details pane')), context);
  for (const types of [[false],[true],[true,false]]) {
    S.rows.clear(); types.forEach((torrent,i) => S.rows.set(i ? "b" : "a", {id:i?"b":"a",torrent,status:"Paused"}));
    await context.rowMenu(0,0);
    const bandwidth = items.find(item => item.label === "Bandwidth limit");
    assert.deepEqual(Array.from(bandwidth.sub.filter(Boolean),item=>item.label), types.every(Boolean) ? ["Download","Upload","Seeding"] : ["Download"]);
    bandwidth.sub[0].sub[1].click();
    assert.equal(patches.at(-1)[1].patch.limitKBps,1024);
    assert.deepEqual(Array.from(patches.at(-1)[1].ids),[...S.rows.keys()]);
  }
});

test("wide banner sizing publishes the aspect and resets it when artwork changes", async () => {
  const pending = [];
  class Image { set src(url) { pending.push(this); } }
  const context = vm.createContext({ Image });
  const common = source("common.js");
  vm.runInContext(common.slice(common.indexOf("const bannerSizeCache"), common.indexOf("function sampleCardArt")), context);
  const classes = new Set(), props = {}, parentProps = {};
  const element = { dataset: {}, querySelector: () => null,
    parentElement: { style: { setProperty: (k,v) => parentProps[k] = v, removeProperty: k => delete parentProps[k] } },
    classList: { contains: c => classes.has(c), remove: c => classes.delete(c), toggle: (c,on) => on ? classes.add(c) : classes.delete(c) },
    style: { setProperty: (k,v) => props[k] = v } };
  context.sizeCardBanner(element,"wide");
  Object.assign(pending[0], {naturalWidth:1920,naturalHeight:620});pending[0].onload();await Promise.resolve();
  assert.ok(classes.has("aspect-banner"));assert.equal(parentProps["--banner-ratio"],1920/620);
  classes.add("icon-wash");context.sizeCardBanner(element,"icon");
  assert.ok(!classes.has("aspect-banner"));assert.equal(parentProps["--banner-ratio"],undefined);
});


test("splitters capture a pointer and persist once after release or capture loss", () => {
  const main = source("main.js"), events = {}, classes = new Set(); let captured = null, saved = 0, ended = 0, x = 0;
  const handle = { addEventListener: (k,v) => events[k] = v,
    classList: { add: k => classes.add(k), remove: k => classes.delete(k) },
    setPointerCapture: id => captured = id, hasPointerCapture: id => captured === id,
    releasePointerCapture: () => captured = null };
  const context = vm.createContext({ saveLayout: () => saved++ });
  vm.runInContext(main.slice(main.indexOf('function splitter('),main.indexOf('// ---------------------------------------------------------------- sidebar')),context);
  context.splitter(handle,e=>x=e.clientX,()=>ended++);
  const e = {button:0,pointerId:7,preventDefault(){}};
  events.pointerdown(e); assert.equal(captured,7); assert.ok(classes.has('drag'));
  events.pointermove({...e,clientX:200}); assert.equal(x,200);
  events.pointermove({...e,pointerId:8,clientX:400}); assert.equal(x,200);
  events.pointerup(e); events.lostpointercapture(e); assert.equal(saved,1); assert.equal(ended,1); assert.ok(!classes.has('drag'));
  events.pointerdown(e); events.pointercancel(e); assert.equal(saved,2);
  events.pointerdown(e); events.lostpointercapture(e); assert.equal(saved,3);
});


test("art treatment resets screenshot dimming and validates publisher tint", () => {
  const common = source("common.js"), classes = new Set(["icon-wash"]), props = {};
  const context=vm.createContext({sizeCardBanner:()=>{}});
  vm.runInContext(common.slice(common.indexOf('function sampleCardArt('),common.indexOf('function sampleCardArt(')+common.slice(common.indexOf('function sampleCardArt(')).indexOf('\n}\n')+3),context);
  const element={dataset:{artColor:'#E95420'},querySelector:()=>null,classList:{contains:c=>classes.has(c),toggle:(c,on)=>on?classes.add(c):classes.delete(c)},style:{setProperty:(k,v)=>props[k]=v}};
  context.sampleCardArt(element,'http://img.localhost/b/test?v=1&texture=1');
  assert.ok(classes.has('screenshot-art')); assert.ok(classes.has('tinted-wash')); assert.equal(props['--art-tint'],'#E95420');
  element.dataset.artColor='red;display:none';context.sampleCardArt(element,'normal');
  assert.ok(!classes.has('screenshot-art')); assert.ok(!classes.has('tinted-wash')); assert.equal(props['--art-tint'],'transparent');
});


test("program banners retain their category and stay outside the Games filter", () => {
  const main=source('main.js'),context=vm.createContext({S:{nav:'games'}});
  vm.runInContext(main.slice(main.indexOf('function matchesNav('),main.indexOf('function matches(r)')),context);
  vm.runInContext(main.slice(main.indexOf('function detailType('),main.indexOf('function renderDetailHeader(')),context);
  const program={game:false,banner:'banner',cover:'cover',category:'Programs'};
  assert.equal(context.matchesNav(program),false);assert.equal(context.detailType(program),'program');
  assert.equal(context.matchesNav({...program,game:true}),true);assert.equal(context.detailType({...program,game:true}),'game');
});

test("torrent files form a folder tree with one top row and combined totals", () => {
  const main = source("main.js"), S = {};
  const context = vm.createContext({ S });
  vm.runInContext(main.slice(main.indexOf("function fileTree"), main.indexOf("function renderTorrentTab")), context);
  const one = context.fileTree([{ index: 0, path: "Moo Who/Moo.Who.exe" }, { index: 1, path: "Moo Who/Fix Repair/fix.dll" }], "x");
  assert.equal(one.length, 1); assert.equal(one[0].name, "Moo Who"); assert.equal(one[0].all.length, 2);
  assert.ok(one[0].kids[0].folder, "folders sort before files");
  const loose = context.fileTree([{ index: 0, path: "a.bin" }, { index: 1, path: "b.bin" }], "Linux ISO");
  assert.equal(loose.length, 1); assert.equal(loose[0].name, "Linux ISO"); assert.equal(loose[0].all.length, 2);
  const single = context.fileTree([{ index: 0, path: "movie.mkv" }], "movie");
  assert.equal(single.length, 1); assert.equal(single[0].folder, false);
  assert.equal(context.fileTreeClosed("a"), context.fileTreeClosed("a"));
});
test("force reannounce reports paused-torrent errors and preserves successful requests", async () => {
  const main = source("main.js");
  const start = main.indexOf("  async reannounce() {");
  const end = main.indexOf("\n  async setLocation()", start);
  for (const result of [1, 0, new Error("Start it first")]) {
    const alerts = [], calls = [];
    const context = vm.createContext({
      selIds: () => ["torrent"],
      invoke: async (name, args) => { calls.push([name, args.ids.join(",")]); if (result instanceof Error) throw result; return result; },
      alertBox: (...args) => alerts.push(args),
    });
    vm.runInContext("globalThis.actions = {" + main.slice(start, end) + "};", context);
    await context.actions.reannounce();
    assert.deepEqual(calls, [["torrent_reannounce", "torrent"]]);
    assert.equal(alerts.length, result === 1 ? 0 : 1);
    if (result instanceof Error) assert.match(alerts[0][1], /Start it first/);
  }
});

test("torrent engine status text reaches the status pill unchanged", () => {
  const main = source("main.js"), context = vm.createContext({ esc: String });
  vm.runInContext(main.slice(main.indexOf("function statusClass("), main.indexOf("function renderRow(")), context);
  for (const row of [
    {status: "Verifying", statusText: "Checking 37%"},
    {status: "Paused", statusText: "Paused � Files missing - Force recheck to download them again"},
    {status: "Downloading", statusText: "Downloading"},
    {status: "Completed", statusText: "Complete � seeding", seeding: true},
  ]) {
    const html = context.cellHtml("Status", row);
    assert.ok(html.includes('>' + row.statusText + '</span>'));
    assert.ok(html.includes(row.seeding ? 'st-Seeding' : 'st-' + row.status));
  }
});

test("checking torrents offer Pause and keep checking progress in cards and details", () => {
  const main = source("main.js"), context = vm.createContext({});
  vm.runInContext(main.slice(main.indexOf("function statusLabel("), main.indexOf("function cellHtml(")), context);
  const checking = { torrent: true, status: "Verifying", statusText: "Checking 37%", running: false, seeding: false };
  assert.equal(context.statusLabel(checking), "Checking 37%");
  assert.equal(context.canPause(checking), true);
  assert.equal(context.canPause({ ...checking, status: "Paused" }), false);
  assert.equal(context.statusLabel({ ...checking, status: "Completed", seeding: true }), "Seeding");
  assert.equal(context.statusLabel({ ...checking, torrent: false }), "Verifying");
  assert.match(main, /#dStatus"\)\.textContent = statusLabel\(r\)/);
  assert.match(main, /const status = statusLabel\(r\)/);
});

// Remembering the Add selection is independent of submitting or cancelling the form.
test("Add restores a valid queue and remembers queue and None changes", async () => {
  const add = source("add.js");
  const block = add.slice(add.indexOf('  $("#queue").replaceChildren'), add.indexOf('  $("#headers").value'));
  for (const remembered of ["Night", "removed", ""]) {
    const calls = [];
    const queue = { value: "", events: {}, replaceChildren(...kids) { this.options = kids; }, addEventListener(ev, fn) { this.events[ev] = fn; } };
    const context = vm.createContext({ d: { queues: ["Main", "Night"], lastQueue: remembered, request: {} },
      $: () => queue, el: (tag, props) => ({ tag, ...props }),
      invoke: async (cmd, args) => calls.push([cmd, args.name]),
    });
    vm.runInContext(block, context);
    assert.equal(queue.value, remembered === "Night" ? "Night" : "");
    assert.equal(queue.options.length, 3);
    queue.value = "Main";
    await queue.events.change();
    queue.value = "";
    await queue.events.change();
    assert.deepEqual(calls, [["remember_add_queue", "Main"], ["remember_add_queue", ""]]);
  }
});

test("scheduler saves idle and retry controls with numeric count and delay", async () => {
  const ui = dialogContext();
  ui.Element.prototype.replaceChildren = function (...kids) { this.children = []; this.append(...kids); };
  ui.document.createTextNode = (text) => Object.assign(new ui.Element("text"), { textContent: String(text) });
  const q = { Name: "Night", Members: [], Running: false, MaxConcurrent: 2, SpeedLimitKBps: 0,
    StopWhenEmpty: true, RetryCount: 0, RetryDelaySec: 20, StartAt: "02:00", StopAt: "08:00", DaysMask: 127, PostAction: "None" };
  const calls = [];
  ui.context.window.__TAURI__.core = { invoke: async (cmd, args) => { calls.push([cmd, args]); return cmd === "queues" ? [q] : undefined; } };
  ui.context.S = { nav: "all" };
  ui.context.speedCombo = () => {};
  const main = source("main.js");
  vm.runInContext(main.slice(main.indexOf("function scheduleTime("), main.indexOf("// ---------------------------------------------------------------- dialogs: stats")), ui.context);
  await ui.context.openScheduler();
  const nodes = [];
  const visit = (node) => { nodes.push(node); node.children.forEach(visit); };
  visit(ui.document.body);
  const inputs = nodes.filter((n) => n.tagName === "INPUT");
  const count = inputs.find((n) => n.type === "number" && n.max === 100);
  const delay = inputs.find((n) => n.type === "number" && n.max === 86400);
  assert.ok(count); assert.ok(delay);
  count.value = "3"; await count.events.change();
  delay.value = "45"; await delay.events.change();
  const label = nodes.find((n) => n.tagName === "LABEL" && n.children.some((c) => c.textContent === "Stop the queue when it is empty"));
  label.children[0].checked = false; await label.children[0].events.change();
  const def = calls.filter(([cmd]) => cmd === "queue_save").at(-1)[1].def;
  assert.equal(def.RetryCount, 3); assert.equal(def.RetryDelaySec, 45); assert.equal(def.StopWhenEmpty, false);
});


test("downloads waiting for a manual slot offer Pause", () => {
  const main = source("main.js"), context = vm.createContext({});
  vm.runInContext(main.slice(main.indexOf("function statusLabel("), main.indexOf("function cellHtml(")), context);
  assert.equal(context.canPause({ status: "Queued", torrent: false, running: false }), true);
  assert.equal(context.canPause({ status: "Paused", torrent: false, running: false }), false);
});

function schedulerNodes(ui) {
  const nodes = [];
  const visit = node => { nodes.push(node); node.children.forEach(visit); };
  visit(ui.document.body);
  return nodes;
}
async function schedulerHarness(overrides = {}) {
  const ui = dialogContext();
  ui.Element.prototype.replaceChildren = function (...kids) { this.children = []; this.append(...kids); };
  ui.document.createTextNode = text => Object.assign(new ui.Element("text"), { textContent: String(text) });
  const q = { Name: "Night", Members: ["a", "b", "c", "d"].map(id => ({ id, name: id, size: "1 MB", status: "Paused" })),
    Running: false, MaxConcurrent: 2, SpeedLimitKBps: 0, StopWhenEmpty: true, RetryCount: 0, RetryDelaySec: 20,
    StartAtEnabled: true, StartAt: "02:30", StopAtEnabled: true, StopAt: "07:30", Daily: true, DaysMask: 62, OnceDate: "2026-11-02T00:00:00", PostAction: "None", ...overrides };
  let server = structuredClone(q);
  const calls = [];
  ui.context.window.__TAURI__.core = { invoke: async (cmd, args) => {
    calls.push([cmd, structuredClone(args)]);
    if (cmd === "queues") return [structuredClone(server)];
    if (cmd === "queue_save") server = { ...server, ...structuredClone(args.def) };
    if (cmd === "queue_run") server.Running = args.on;
    if (cmd === "queue_move") { const from = server.Members.findIndex(m => m.id === args.id); server.Members.splice(from + args.dir, 0, ...server.Members.splice(from, 1)); }
    if (cmd === "set_items") server.Members = server.Members.filter(m => !args.ids.includes(m.id));
  } };
  ui.context.S = { nav: "all" };
  ui.context.speedCombo = () => {};
  vm.runInContext('confirmBox = async () => true;', ui.context);
  const main = source("main.js");
  vm.runInContext(main.slice(main.indexOf("function scheduleTime("), main.indexOf("// ---------------------------------------------------------------- dialogs: stats")), ui.context);
  await ui.context.openScheduler();
  return Object.assign(ui, { calls, server: () => server, nodes: () => schedulerNodes(ui),
    button: text => schedulerNodes(ui).find(n => n.tagName === "BUTTON" && n.textContent === text),
    input: label => schedulerNodes(ui).find(n => n["aria-label"] === label),
    tick: () => [...ui.intervals.values()][0](),
    rows: () => schedulerNodes(ui).filter(n => n.role === "option"),
  });
}

test("scheduler sections, day shortcuts, time toggles and one-off date stay saved", async () => {
  const ui = await schedulerHarness();
  assert.deepEqual(ui.nodes().filter(n => n.tagName === "H3").map(n => n.textContent), ["Name", "When", "Limits", "When the queue finishes", "Items"]);
  assert.equal(ui.nodes().find(n => n.className === "sched-summary").textContent, "Starts every weekday at 02:30, stops at 07:30.");
  for (const [name, mask] of [["Weekends", 65], ["Every day", 127], ["Weekdays", 62]]) {
    await ui.button(name).click();
    assert.equal(ui.server().DaysMask, mask);
  }
  const startLabel = ui.nodes().find(n => n.tagName === "LABEL" && n.children.some(c => c.textContent === "Start at"));
  startLabel.children[0].checked = false; await startLabel.children[0].events.change();
  assert.equal(ui.input("Start time").disabled, true);
  assert.equal(ui.server().StartAtEnabled, false);
  const mode = ui.input("Schedule repetition"); mode.value = "once"; await mode.events.change();
  const date = ui.input("One-off date"); assert.equal(date.hidden, false);
  date.value = "2026-12-10"; await date.events.change();
  assert.equal(ui.server().Daily, false); assert.equal(ui.server().OnceDate, "2026-12-10T00:00:00");
  mode.value = "daily"; await mode.events.change();
  assert.equal(date.hidden, true); assert.equal(ui.server().DaysMask, 62);
});

test("scheduler preview honours overnight slots, fired guards and distant one-off dates", () => {
  const main = source("main.js"), ctx = vm.createContext({});
  vm.runInContext(main.slice(main.indexOf("function scheduleTime("), main.indexOf("async function openScheduler(")), ctx);
  const q = { StartAtEnabled: true, StartAt: "02:30", StopAtEnabled: true, StopAt: "07:30", Daily: true, DaysMask: 62 };
  assert.equal(ctx.scheduleNextStart(q, new Date(2026, 10, 2, 0, 0)), "Mon 02:30 - in 2 h 30 m");
  assert.equal(ctx.scheduleNextStart(q, new Date(2026, 10, 2, 3)), "Start due now");
  assert.equal(ctx.scheduleNextStart({ ...q, LastStartSlot: "2026-11-02" }, new Date(2026, 10, 2, 3)), "Tue 02:30 - in 23 h 30 m");
  const overnight = { ...q, StartAt: "22:00", StopAt: "02:00", DaysMask: 2 };
  assert.equal(ctx.scheduleNextStart(overnight, new Date(2026, 10, 3, 1)), "Start due now");
  assert.match(ctx.scheduleSummary(overnight), /stops at 02:00 the next day/);
  assert.match(ctx.scheduleNextStart(overnight, new Date(2026, 10, 3, 2)), /^Mon 22:00/);
  assert.equal(ctx.scheduleNextStart({ ...q, DaysMask: 0 }), "No scheduled days");
  assert.equal(ctx.scheduleNextStart({ ...q, StartAtEnabled: false }), "No automatic start");
  const once = { ...q, Daily: false, OnceDate: "2026-12-10T00:00:00" };
  assert.match(ctx.scheduleNextStart(once, new Date(2026, 10, 2)), /^Thu 02:30 - in /);
  assert.equal(ctx.scheduleNextStart(once, new Date(2026, 11, 11)), "One-off start has passed");
});

test("scheduler supports ordered moves, drag, removal and clearing without deleting downloads", async () => {
  const ui = await schedulerHarness();
  assert.equal(ui.button("Move to top").disabled, true);
  ui.rows()[3].click(); await ui.button("Move to top").click();
  assert.deepEqual(ui.server().Members.map(m => m.id), ["d", "a", "b", "c"]);
  await ui.button("Move to bottom").click();
  assert.deepEqual(ui.server().Members.map(m => m.id), ["a", "b", "c", "d"]);
  ui.rows()[1].click(); await ui.button("Move down").click(); await ui.button("Move up").click();
  assert.deepEqual(ui.server().Members.map(m => m.id), ["a", "b", "c", "d"]);
  const rows = ui.rows(), transfer = { setData(type, id) { this.id = id; } };
  rows[0].events.dragstart({ dataTransfer: transfer });
  let prevented = false;
  rows[2].events.dragover({ dataTransfer: transfer, preventDefault() { prevented = true; } });
  assert.equal(prevented, true);
  rows[2].events.drop({ preventDefault() {} });
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(ui.server().Members.map(m => m.id), ["b", "c", "a", "d"]);
  assert.equal(transfer.id, "a");
  await ui.button("Remove from queue").click();
  assert.deepEqual(ui.server().Members.map(m => m.id), ["b", "c", "d"]);
  await ui.button("Clear queue").click();
  assert.equal(ui.server().Members.length, 0);
  assert.equal(ui.button("Clear queue").disabled, true);
  assert.ok(ui.calls.filter(([cmd]) => cmd === "set_items").every(([, args]) => args.patch.queue === ""));
  assert.equal(ui.calls.some(([cmd]) => /delete|remove_items/.test(cmd)), false);
});

test("scheduler live state refresh preserves the editor and stops polling on close", async () => {
  const ui = await schedulerHarness();
  assert.equal(ui.button("Start queue").disabled, false); assert.equal(ui.button("Queue stopped").disabled, true);
  const name = ui.input("Name"); name.value = "Unfinished edit"; name.focus();
  ui.server().Running = true; ui.server().Members[0].status = "Downloading";
  await ui.tick();
  assert.equal(ui.input("Name"), name); assert.equal(name.value, "Unfinished edit"); assert.equal(ui.document.activeElement, name);
  assert.equal(ui.button("Queue running").disabled, true); assert.equal(ui.button("Stop queue").disabled, false);
  assert.ok(ui.nodes().some(n => n.textContent === "1 MB · Downloading"));
  await ui.button("Stop queue").click(); assert.equal(ui.server().Running, false);
  await ui.button("Start queue").click(); assert.equal(ui.server().Running, true);
  assert.equal(ui.intervals.size, 1);
  await ui.button("Close").click(); assert.equal(ui.intervals.size, 0);
});

test("scheduler ignores stale refreshes after actions and after closing", async () => {
  const ui = await schedulerHarness();
  const core = ui.context.window.__TAURI__.core, original = core.invoke;
  let release;
  const stale = [structuredClone(ui.server())];
  core.invoke = (cmd, args) => cmd === "queues" && !release ? new Promise(resolve => { release = () => resolve(stale); }) : original(cmd, args);
  const refreshing = ui.tick();
  await ui.button("Start queue").click();
  release(); await refreshing;
  assert.equal(ui.button("Queue running").disabled, true);
  assert.equal(ui.button("Stop queue").disabled, false);
  release = null;
  const finalRefresh = ui.tick();
  await ui.button("Close").click(); release(); await finalRefresh;
  assert.equal(ui.document.body.children.length, 0); assert.equal(ui.intervals.size, 0);
});

test("scheduler serializes saves, recovers failed edits and keeps keyboard selection", async () => {
  const ui = await schedulerHarness();
  const core = ui.context.window.__TAURI__.core, original = core.invoke;
  let release, saves = 0;
  core.invoke = async (cmd, args) => {
    if (cmd === "queue_save") {
      saves++;
      if (saves === 1) await new Promise(resolve => { release = resolve; });
    }
    return original(cmd, args);
  };
  const count = ui.input("RetryCount"), delay = ui.input("RetryDelaySec");
  count.value = "2"; const first = count.events.change();
  delay.value = "40"; const second = delay.events.change();
  await Promise.resolve(); assert.equal(saves, 1);
  assert.equal(ui.button("Start queue").disabled, true);
  release(); await Promise.all([first, second]);
  assert.equal(saves, 2); assert.equal(ui.server().RetryCount, 2); assert.equal(ui.server().RetryDelaySec, 40);
  const row = ui.rows()[1]; row.focus();
  row.events.keydown({ key: " ", preventDefault() {} });
  assert.equal(ui.rows()[1]["aria-selected"], "true");
  ui.server().Members[1].status = "Queued"; await ui.tick();
  assert.equal(ui.document.activeElement, ui.rows()[1]);
  vm.runInContext('alertBox = (title, message) => { globalThis.lastError = message; };', ui.context);
  core.invoke = async (cmd, args) => { if (cmd === "queue_save") throw Error("Save failed"); return original(cmd, args); };
  const name = ui.input("Name"); name.value = "Rejected rename"; await name.events.change();
  assert.match(ui.context.lastError, /Save failed/); assert.equal(ui.input("Name").value, "Night");
  assert.equal(ui.button("Start queue").disabled, false);
});

test("scheduler normalizes saved legacy times for previews and native time inputs", async () => {
  const ui = await schedulerHarness({ StartAt: " 2:3 ", StopAt: "7" });
  assert.equal(ui.input("Start time").value, "02:03");
  assert.equal(ui.input("Stop time").value, "07:00");
  assert.equal(ui.nodes().find(n => n.className === "sched-summary").textContent, "Starts every weekday at 02:03, stops at 07:00.");
  assert.equal(ui.context.scheduleNextStart({ ...ui.server(), StartAt: "2:30" }, new Date(2026, 10, 2)), "Mon 02:30 - in 2 h 30 m");
});


test("finish countdown reports a blocked action when downloads start before expiry", async () => {
  let dialog, tick; const errors = [], calls = [];
  const context = vm.createContext({
    listen: async () => () => {}, el: () => ({}),
    modal: options => { dialog = options; return { close: value => options.onClose(value) }; },
    setInterval: fn => { tick = fn; return 1; }, clearInterval() {},
    alertBox: (title, message) => errors.push({ title, message }),
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (command === "run_post_action") throw "Other downloads are still running or waiting. The finish action was skipped.";
    },
  });
  const main = source("main.js"), start = main.indexOf("function showCountdown(");
  vm.runInContext(main.slice(start, main.indexOf("\nasync function offerFirewall", start)), context);
  context.showCountdown({ action: "Sleep", queue: "Night", force: true });
  for (let i = 0; i < 60; i++) tick();
  await Promise.resolve(); await Promise.resolve();
  assert.equal(calls.filter(c => c.command === "run_post_action").length, 1);
  assert.equal(errors.length, 1); assert.equal(errors[0].title, "Finish action skipped");
  assert.match(errors[0].message, /Other downloads/);
  dialog.onClose("now"); assert.equal(calls.filter(c => c.command === "run_post_action").length, 1);
});


test("file conflict choices save an item decision before restarting and Skip stays paused", async () => {
  const main = source("main.js");
  for (const policy of ["Number", "Overwrite", "Skip", undefined]) {
    const calls = []; let dialog;
    const c = vm.createContext({ el: () => ({}), modal: opts => { dialog = opts; opts.onClose(policy); }, invoke: async (cmd,args) => calls.push([cmd,args]) });
    vm.runInContext(main.slice(main.indexOf("async function showFileConflict("), main.indexOf("// ---------------------------------------------------------------- start-up")), c);
    await c.showFileConflict({id:"item",name:"file.zip"});
    assert.deepEqual(Array.from(dialog.buttons, b => b.label), ["Skip", "Add a number", "Overwrite"]);
    assert.equal(calls.length, policy ? (policy === "Skip" ? 1 : 2) : 0);
    if (policy) { assert.equal(calls[0][0], "set_items"); assert.equal(calls[0][1].patch.existingFilePolicy, policy); }
    if (policy && policy !== "Skip") assert.equal(calls[1][0], "start");
  }
});

test("download settings expose proxy safety, unfinished folder, collision policy and all speed days", () => {
  const main = source("main.js");
  for (const key of ["ProxyMode", "ProxyHost", "ProxyPort", "ProxyUsername", "ProxyPassword", "ProxyBypassLocal", "IncompleteFolder", "ExistingFilePolicy"])
    assert.ok(main.includes('("' + key + '"'), key);
  assert.ok(main.includes('text("ProxyPassword", { type: "password"'));
  assert.match(main, /DHT and UDP trackers still connect directly/);
  assert.match(main, /Torrent proxy changes take effect after restarting/);
  assert.ok(main.includes('ScheduleDaysMask ?? 127'));
  assert.ok(main.includes('if (k.startsWith("speedDay")) continue'));
});

test("torrent Add keeps excluded defaults unticked and lets users change them", async () => {
  const elements = new Map();
  const context = vm.createContext({
    A: { gen: 1, cls: {}, init: { seedRatio: 1 } },
    $: key => { if (!elements.has(key)) elements.set(key, { value: "", hidden: false }); return elements.get(key); },
    invoke: async () => ({ name: "Torrent", size: 300, files: [
      { index: 0, path: "ReadMe.TXT", length: 100, wanted: false },
      { index: 1, path: "movie.mkv", length: 100, wanted: true },
      { index: 2, path: "old-server.bin", length: 100 },
    ] }),
    setName() {}, setInfo() {}, buildTree: files => files, walkDirs() {}, drawTorrentFiles() {}, growForTorrent() {}, freeWarning: async () => "", fmtSize: n => String(n),
  });
  const add = source("add.js"), start = add.indexOf("async function refreshTorrent(");
  vm.runInContext(add.slice(start, add.indexOf("// ---------------------------------------------------------------- torrent file tree", start)), context);
  await context.refreshTorrent(1, ["file.torrent"]);
  const files = context.A.torrentFiles;
  assert.deepEqual(Array.from(files, f => f.on), [false, true, true]);
  assert.deepEqual(Array.from(files[0].parts), ["ReadMe.TXT"]);
  files[0].on = true;
  assert.equal(files.every(f => f.on), true);
});

test("torrent settings collect multiline lists, time limit, action and marker", () => {
  const main = source("main.js");
  for (const key of ["TorrentSeedMinutes", "TorrentSeedAction", "TorrentAlwaysTrackers", "TorrentSkipPatterns", "TorrentMarkUnfinished"])
    assert.ok(main.includes('("' + key + '"') || main.includes('s.' + key), key);
  const f = {
    TorrentSeedMinutes: { type: "number", value: "45" },
    TorrentSeedRatio: { type: "number", value: "2" },
    TorrentSeedAction: { type: "select-one", value: "Remove" },
    TorrentAlwaysTrackers: { type: "textarea", value: "https://tracker.invalid/a\nudp://tracker.invalid:80/a" },
    TorrentSkipPatterns: { type: "textarea", value: "*.txt\n*sample*" },
    TorrentMarkUnfinished: { type: "checkbox", checked: true },
  };
  const c = vm.createContext({ s: {}, f, structuredClone, speedDays: [], glow: { value: 70 }, conn: { value: 8 }, fmtSel: {}, cookieSel: {}, themeSel: {} });
  const start = main.indexOf("  function collect() {");
  vm.runInContext(main.slice(start, main.indexOf("\n  let group;", start)), c);
  const result = c.collect();
  assert.equal(result.TorrentSeedMinutes, 45); assert.equal(result.TorrentSeedRatio, 2);
  assert.equal(result.TorrentSeedAction, "Remove"); assert.equal(result.TorrentMarkUnfinished, true);
  assert.equal(result.TorrentAlwaysTrackers, f.TorrentAlwaysTrackers.value);
  assert.equal(result.TorrentSkipPatterns, f.TorrentSkipPatterns.value);
});
test("torrent discovery, watch and blocklist settings persist through collection", () => {
  const main = source("main.js");
  for (const key of ["TorrentWatchFolder", "TorrentBlocklistUrl", "TorrentEnableDht", "TorrentEnableLocalDiscovery", "TorrentIpv4Only"])
    assert.ok(main.includes('("' + key + '"'), key);
  assert.match(main, /refreshed weekly/);
  assert.match(main, /changes apply after restarting FluxDM/);
  const fields = {
    TorrentSeedRatio: { type: "number", value: "1" },
    TorrentWatchFolder: { type: "text", value: "watched" },
    TorrentBlocklistUrl: { type: "text", value: "https://example.invalid/list.gz" },
    TorrentEnableDht: { type: "checkbox", checked: false },
    TorrentEnableLocalDiscovery: { type: "checkbox", checked: false },
    TorrentIpv4Only: { type: "checkbox", checked: true },
  };
  const c = vm.createContext({ s: {}, f: fields, structuredClone, speedDays: [], glow: { value: 70 }, conn: { value: 8 }, fmtSel: {}, cookieSel: {}, themeSel: {} });
  const start = main.indexOf("  function collect() {");
  vm.runInContext(main.slice(start, main.indexOf("\n  let group;", start)), c);
  const result = c.collect();
  assert.equal(result.TorrentWatchFolder, "watched");
  assert.equal(result.TorrentBlocklistUrl, fields.TorrentBlocklistUrl.value);
  assert.equal(result.TorrentEnableDht, false);
  assert.equal(result.TorrentEnableLocalDiscovery, false);
  assert.equal(result.TorrentIpv4Only, true);
});
