// Tells the app this page is really drawing frames, so a window that silently stops painting can be
// nudged back to life (see webview_guard.rs). One frame is requested per check instead of a
// requestAnimationFrame loop: a loop keeps the renderer ticking at the monitor's refresh rate, even
// while the window is hidden (the app turns WebView2's background throttling off).
(() => {
  const T = window.__TAURI__;
  if (!T || !T.core) return;
  setInterval(() => {
    if (document.hidden) return;
    requestAnimationFrame(() => T.core.invoke("ui_heartbeat").catch(() => {}));
  }, 4000);
})();
