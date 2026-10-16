// The small notification card in the bottom-right corner. Click it to open the file's folder (or FluxDM).
// This page decides what is on screen: it asks Rust to show the window for a toast (toast_show) and to hide it
// when it's done (toast_hide). If the page ever goes quiet, Rust hides the window and sends "toast-reset".
"use strict";

let reveal = null, hideAt = 0, paused = false, shownAt = 0, current = null;
const queue = [];
const SHOW_MS = 6500, MIN_MS = 2500, MAX_HOVER_MS = 15000;
$("#x").innerHTML = icon("x");

invoke("get_settings").then((g) => applyTheme(g.settings.Theme)).catch(() => {});

const same = (a, b) => a && b && a.title === b.title && a.body === b.body;

// A toast arriving while another is on screen waits its turn (the one showing is cut short, but never under
// MIN_MS) instead of replacing it mid-read; identical repeats are dropped.
function receive(p) {
  if (same(queue[queue.length - 1] || current, p)) return;
  if (current) {
    queue.push(p);
    hideAt = Math.min(hideAt, Math.max(shownAt + MIN_MS, Date.now() + 1500));
  } else show(p);
}
listen("toast", receive).then(() => invoke("toast_ready")).then((waiting) => (waiting || []).forEach(receive));

// Rust hid the window because this page stopped answering: forget the stale card and carry on with the rest.
listen("toast-reset", () => {
  current = null;
  hideAt = 0;
  if (queue.length) show(queue.shift());
});

function show(p) {
  current = p;
  paused = false; // the mouse can't "leave" a window that was hidden under it
  shownAt = Date.now();
  $("#title").textContent = p.title || "FluxDM";
  $("#body").textContent = p.body || "";
  reveal = p.reveal || null;
  $("#t").title = reveal ? "Click to show it in its folder" : "Click to open FluxDM";
  hideAt = Date.now() + SHOW_MS;
  const bar = $("#timer");
  bar.style.transition = "none";
  bar.style.transform = "scaleX(1)";
  requestAnimationFrame(() => requestAnimationFrame(() => { bar.style.transition = `transform ${SHOW_MS}ms linear`; bar.style.transform = "scaleX(0)"; }));
  invoke("toast_show", { gen: p.gen || 0 });
}

function dismiss() {
  const gen = current ? current.gen || 0 : 0;
  hideAt = 0;
  queue.length = 0;
  current = null;
  invoke("toast_hide", { gen });
}

setInterval(() => {
  if (!current) return;
  // Hovering holds the card, but not forever: a missed mouseleave must not pin it on screen.
  if (paused && Date.now() - shownAt < MAX_HOVER_MS) return;
  if (Date.now() <= hideAt) return;
  if (queue.length) show(queue.shift());
  else dismiss();
}, 250);
$("#t").addEventListener("mouseenter", () => { paused = true; });
$("#t").addEventListener("mouseleave", () => { paused = false; hideAt = Math.max(hideAt, Date.now() + 2000); });
$("#t").addEventListener("click", (e) => { if (e.target.closest("#x")) return; const r = reveal; dismiss(); invoke("toast_click", { reveal: r }); });
$("#x").addEventListener("click", dismiss);
