import { ext, loadSettings, api } from "./common.js";

const $ = id => document.getElementById(id);
const toggles = ["enabled", "autostart", "allTypes", "sendCookies", "sendUserAgent"];
const fields = ["exts", "bigFileMB", "port"];
const ALL_SITES = { origins: ["<all_urls>"] };

async function init() {
  const s = await loadSettings();
  for (const k of toggles) {
    $(k).checked = !!s[k];
    $(k).addEventListener("change", () => { save(k, $(k).checked); listState(); });
  }
  const listState = () => { $("extsLabel").classList.toggle("dim", $("allTypes").checked); };
  listState();
  for (const k of fields) {
    $(k).value = s[k];
    $(k).addEventListener("change", () => {
      const v = $(k).type === "number" ? Math.max(0, parseInt($(k).value, 10) || 0) : $(k).value.trim();
      save(k, v);
    });
  }

  $("grab").addEventListener("click", async () => {
    say("Looking for links…");
    const r = await ext.runtime.sendMessage({ cmd: "grab" });
    say(r?.count ? `Sent ${r.count} link${r.count > 1 ? "s" : ""} to FluxDM.` : "Nothing sent.");
  });
  $("video").addEventListener("click", async () => {
    const r = await ext.runtime.sendMessage({ cmd: "video" });
    say(r?.ok ? "Sent to FluxDM as a video download." : "Couldn't send this page.");
  });
  $("sendPage").addEventListener("click", async () => {
    const r = await ext.runtime.sendMessage({ cmd: "sendPage" });
    say(r?.ok ? "Sent to FluxDM." : "Couldn't send this page.");
  });
  $("skipNext").addEventListener("click", async () => {
    await ext.storage.local.set({ skipNextUntil: Date.now() + 60_000 });
    say("Your next download (within 1 minute) stays in the browser.");
  });
  $("grant").addEventListener("click", async () => {
    if (await ext.permissions.request(ALL_SITES)) $("perm").hidden = true;
  });

  // Firefox/Waterfox let people withhold site access; downloads still get caught, but cookies need it.
  try { $("perm").hidden = await ext.permissions.contains(ALL_SITES); } catch { }

  checkStatus(s);
  showActivity();
  if (location.hash === "#activity") $("activityBox").open = true;
}

async function showActivity() {
  const { activity = [] } = await ext.storage.local.get("activity");
  const list = $("activity");
  list.replaceChildren();
  if (activity.length === 0) {
    const li = document.createElement("li");
    li.textContent = "No downloads seen yet.";
    list.append(li);
    return;
  }
  for (const a of activity) {
    const li = document.createElement("li");
    const b = document.createElement("b");
    b.textContent = a.what;
    b.title = a.what;
    const span = document.createElement("span");
    span.textContent = `${new Date(a.t).toLocaleTimeString()} · ${a.result}`;
    li.append(b, span);
    list.append(li);
  }
}

async function save(key, value) {
  await ext.storage.local.set({ [key]: value });
  if (key === "port") checkStatus(await loadSettings());
}

async function checkStatus(s) {
  const pill = $("status");
  try {
    await api("ping", null, s, 1500);
    pill.textContent = "connected";
    pill.className = "pill ok";
  } catch {
    pill.textContent = "FluxDM not running";
    pill.className = "pill bad";
  }
}

function say(text) { $("msg").textContent = text; }

init();
