const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

const source = (name) => fs.readFileSync(path.join(__dirname, "..", "ui", name), "utf8").replace(/\r\n/g, "\n");

function dialogContext() {
  const listeners = new Map();
  const timers = [];
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
  const context = vm.createContext({ document, window: { __TAURI__: {}, addEventListener() {} }, setTimeout: (fn) => timers.push(fn) });
  vm.runInContext(source("common.js"), context);
  return {
    document, context, Element,
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

test("shared theme events update an already open window and restore system mode", () => {
  for (const name of ["main", "add", "toast"]) {
    const handlers = new Map();
    const document = { documentElement: { dataset: {} }, addEventListener() {} };
    const context = vm.createContext({ document, window: { name, addEventListener() {}, __TAURI__: {
      event: { listen: async (event, callback) => handlers.set(event, callback) },
    } } });
    vm.runInContext(source("common.js"), context);
    for (const theme of ["Light", "Dark", "System"]) {
      handlers.get("theme-changed")({ payload: theme });
      assert.equal(document.documentElement.dataset.theme, theme === "System" ? undefined : theme.toLowerCase());
    }
  }
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
  const end = main.indexOf('\n\n  const body =', start);
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
      el: () => ({}),
      modal: (options) => { dialog = options; return { close: (value) => options.onClose(value) }; },
      setInterval: (fn) => { tick = fn; return 1; }, clearInterval() {},
      invoke: (command, args) => calls.push({ command, args }),
    });
    const main = source("main.js");
    const start = main.indexOf("function showCountdown(");
    vm.runInContext(main.slice(start, main.indexOf("\nasync function offerFirewall", start)), context);
    context.showCountdown({ action: "Shutdown", queue: "Downloads", force: false });
    if (expire) for (let i = 0; i < 60; i++) tick();
    else dialog.onClose("now");
    assert.equal(calls.length, 1);
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
  const context = vm.createContext({ S, document: { hidden: false },
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
    const context = vm.createContext({ S, document: { hidden: false },
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
