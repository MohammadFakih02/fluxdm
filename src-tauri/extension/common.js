// Shared by the background script and the popup.
// Firefox/Waterfox expose promise-based `browser`; Chromium exposes promise-based `chrome` in MV3.
export const ext = globalThis.browser ?? globalThis.chrome;
export const isGecko = typeof globalThis.browser !== "undefined" && navigator.userAgent.includes("Gecko/");

export const DEFAULTS = {
  enabled: true,
  autostart: false,
  // Every file the browser would download goes to FluxDM; the list below is only used when this is off
  // (and by "Grab download links", which would otherwise send every link on the page).
  allTypes: true,
  port: 7878,
  exts: "zip rar 7z gz tgz bz2 tbz2 xz txz zst lz lzma lz4 cab arj tar iso img vhd vhdx wim esd dmg pkg exe msi msix msixbundle appx appxbundle apk xapk apks apkm aab ipa deb rpm appimage flatpak snap jar run bin mp4 mkv avi mov webm m4v wmv flv f4v mpg mpeg ts m2ts mts 3gp ogv vob rmvb mp3 flac wav m4a m4b aac ogg oga opus wma aiff aif ape alac mka pdf epub mobi azw azw3 cbz cbr djvu doc docx xls xlsx ppt pptx odt ods odp rtf psd ai eps tif tiff dng cr2 nef arw safetensors gguf ggml ckpt pth pt onnx h5 tflite pb npz torrent pak vpk nsp xci wbfs rom sav ttf otf woff2 sqlite db csv",
  bigFileMB: 50,
  sendCookies: true,
  sendUserAgent: false,
};

export async function loadSettings() {
  return { ...DEFAULTS, ...(await ext.storage.local.get(null)) };
}

export function extSet(s) {
  return new Set(s.exts.split(/[\s,;]+/).map(e => e.replace(/^\./, "").toLowerCase()).filter(Boolean));
}

export function extOf(nameOrUrl) {
  try {
    const path = /^[a-z]+:\/\//i.test(nameOrUrl) ? new URL(nameOrUrl).pathname : nameOrUrl;
    const base = decodeURIComponent(path.split(/[\\/]/).pop() || "");
    const i = base.lastIndexOf(".");
    return i > 0 ? base.slice(i + 1).toLowerCase() : "";
  } catch {
    return "";
  }
}

/** Calls FluxDM's local API. Throws if FluxDM isn't running. */
export async function api(path, body, s, timeoutMs = 2500) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const init = body
      ? { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body), signal: ctrl.signal }
      : { signal: ctrl.signal };
    const r = await fetch(`http://127.0.0.1:${s.port}/api/${path}`, init);
    if (!r.ok) throw new Error(`FluxDM answered ${r.status}`);
    return await r.json();
  } finally {
    clearTimeout(timer);
  }
}
