# FluxDM 2: the whole picture

This explains what FluxDM 2 is, where everything lives, how the pieces fit together, and how to build, release
and maintain it. Other files go deeper:

- `README.md` covers features, installing and the command line.
- `FEATURE_PARITY.md` checks every feature of the old app, with test results.
- `PROJECT_STATE.md` covers the code layout, build notes and testing tricks.
- `src-tauri/vendor/PATCHES.md` lists every change made to the torrent engine.

## 1. What it is

FluxDM is a download manager for Windows. It handles:
- normal downloads, using many connections at once;
- videos from YouTube and 1000+ other sites, through yt-dlp;
- torrents.

The original FluxDM was written in C#/.NET with WPF. FluxDM 2 is a full rewrite:

| | Old (.NET) | FluxDM 2 |
|---|---|---|
| Language | C# / WPF | Rust (engine) + HTML/CSS/JS (window), glued by Tauri 2 |
| Size | .NET runtime + many DLLs | one `FluxDM.exe` (~28 MB), setup ~7 MB |
| Torrent engine | MonoTorrent (forked) | librqbit 9.0.1 (patched, see section 5) |
| Data | `%APPDATA%\FluxDM` | the same folder and the same JSON files: nothing to migrate |

It needs nothing installed except Microsoft Edge WebView2, which Windows 11 already has. The setup installs
WebView2 if it's missing.

## 2. Where things are

**On a PC where FluxDM is installed**

| What | Where |
|---|---|
| Installed app | `%LOCALAPPDATA%\FluxDM\FluxDM.exe` (per-user, no admin) |
| Your data | `%APPDATA%\FluxDM\`: `downloads.json`, `queues.json`, `settings.json`, `stats.json`, plus `torrents\` (torrent resume data), `tools\` (yt-dlp, ffmpeg), `covers\`, `BrowserExtension\`, `error.log` |
| Portable mode | an empty `portable.txt` next to `FluxDM.exe` keeps the data in a `data` folder beside it |

**Build output** (`dist\`, not part of the source): `FluxDM-Setup-<version>.exe`, `FluxDM.exe` (portable),
`FluxDM.exe.sha256`, the extension zips and `share\` (section 7).

**In the source folder**

| Path | What it does |
|---|---|
| `ui/` | The windows: main list, Add dialog, toast popups. Plain HTML/CSS/JS, built into the exe. |
| `src-tauri/src/main.rs` | Startup, tray icon, windows, and every action the UI can ask for. |
| `src-tauri/src/manager.rs` | The download list, queues, scheduler, speed limits, and the 500 ms update loop. |
| `src-tauri/src/http_dl.rs` | The normal-download engine: connections, splitting work, mirrors, checksums. |
| `src-tauri/src/torrent.rs` | Torrents, built on librqbit. |
| `src-tauri/src/video.rs` | Fetches and runs yt-dlp and ffmpeg. |
| `src-tauri/src/api.rs` | The browser bridge (`127.0.0.1:7878`) and the phone remote (port 7879, PIN-protected). |
| `src-tauri/src/ext.rs` | The browser extension that ships inside the exe (section 4). |
| `src-tauri/src/win.rs` | Direct Windows calls: clipboard, firewall, magnet handler, file icons, sleep/shutdown. |
| `src-tauri/src/update.rs` | Self-update from GitHub releases. |
| `src-tauri/vendor/` | The patched torrent engine. |
| `src-tauri/extension/` | The browser extension's source. |

## 3. How a download flows

1. A link arrives. It can come from the browser extension, the clipboard, drag and drop, a bookmarklet, the
   command line, the phone remote, or the Add button.
2. The **Add window** reads the link: file name, size, and whether it's a torrent or a video page.
3. The manager puts the download in a queue. The queue decides when it starts, and the limits decide how fast
   it runs.
4. One of three engines downloads it:
   - **Files:** up to 64 connections each write their own part directly into the file. Progress is saved
     continuously, so a crash or reboot resumes where it stopped. When a connection finishes early, it takes
     half of the slowest connection's remaining work.
   - **Videos:** yt-dlp downloads and ffmpeg merges. Both are fetched once, on first use.
   - **Torrents:** librqbit, with FluxDM's patches.
5. When it finishes, FluxDM can check the checksum, show a toast and run the queue's end action (sleep,
   shutdown and so on, after a 60 s countdown you can cancel).

## 4. The browser extension

The extension catches download clicks in the browser and hands them to FluxDM, with cookies and referer so
downloads behind a login still work. If FluxDM isn't running, the browser downloads normally.

### Installing it (what users do)

Open **Settings → Browser extension**. FluxDM lists every browser it finds on the PC with an **"Add to …"**
button:

- **Firefox, Waterfox, LibreWolf, Floorp, Zen:** the button opens the extension in that browser. Click **Add**
  in its prompt and you're done. This works in regular Firefox because the extension is **signed by Mozilla**
  (see below).
- **Chrome, Edge, Brave, Opera, Vivaldi:** these browsers only install extensions from their own store, or as
  an "unpacked" folder in Developer mode. The button opens the browser's extensions page and the extension
  folder side by side. Then:
  1. Turn on **Developer mode**.
  2. Click **Load unpacked**.
  3. Pick the `BrowserExtension` folder that opened.

  The folder is `%APPDATA%\FluxDM\BrowserExtension`; keep it there, since the browser loads the extension
  from it.

Tested on 2026-10-01: Firefox installed the signed build. In Chrome 139 the extension loaded with no errors,
and its popup showed **connected** to FluxDM.

### One extension, two flavours

The source in `src-tauri/extension/` serves both browser families. `background.js` checks which browser it's
running in.

| | Firefox family | Chromium family |
|---|---|---|
| Catching downloads | Blocks the response headers before the download starts, and sends the original link, not the expiring redirect | Uses the downloads API: pauses the browser's download and hands it over |
| Manifest | `manifest.json` as written | A cleaned copy without the Firefox-only parts (`webRequestBlocking`, `background.scripts`, `browser_specific_settings`), which Chrome reports as errors |

FluxDM writes the Chromium copy itself when it exports the folder (`ext.rs`, `chromium_manifest`), and
`pack-extension.ps1` does the same for store uploads. A unit test checks the cleaned manifest.

### Mozilla signing (done)

Regular Firefox refuses unsigned extensions. The old `about:config` trick only works in Waterfox, Firefox
Developer, Nightly and ESR. So the extension is signed through the maintainer's account on
**addons.mozilla.org**. Only that account can sign new versions; anyone else changing the extension needs
their own account and their own add-on ID:

- **Distribution: "On your own" (unlisted).** It's signed but not listed in the public store. Signing is
  automatic and took minutes.
- **Add-on ID:** `fluxdm@fluxdm.local`, signed version **1.3.0**.
- The manifest declares `data_collection_permissions: none`. Mozilla requires this for new add-ons, and it's
  true: the extension only talks to FluxDM on `127.0.0.1`.
- The signed file is `src-tauri/extension/signed/FluxDM-extension.xpi`. When that file exists, `build.rs`
  embeds it in the exe, and that's what the Firefox button installs. If it's missing, the build falls back to
  an unsigned copy, which only works in Waterfox, Developer, Nightly and ESR.

### Releasing a new version of the extension

Do this whenever you change anything in `src-tauri/extension/`:

1. Raise `"version"` in `src-tauri/extension/manifest.json`, for example `1.3.0` → `1.3.1`. Stores refuse a
   number they've already seen.
2. Run `.\pack-extension.ps1`. It creates:
   - `dist\FluxDM-extension-firefox.zip`
   - `dist\FluxDM-extension-chromium.zip`
3. On the Add-on Developer Hub: **My Add-ons → FluxDM Integration → Upload a New Version**. Pick the Firefox
   zip. The 3 validation warnings are expected and harmless. Answer **No** to "source code" because nothing
   is minified or bundled.
4. Get the signed file. Firefox *installs* `.xpi` links when clicked, so either:
   - right-click the file link on the version page → **Save Link As…**, or
   - install it in Firefox, then copy
     `%APPDATA%\Mozilla\Firefox\Profiles\<profile>\extensions\fluxdm@fluxdm.local.xpi`. That copy is the
     signed file; it contains `META-INF/cose.sig`.
5. Save it as `src-tauri/extension/signed/FluxDM-extension.xpi`, then rebuild FluxDM (`.\build-setup.ps1`).

Old installs don't update the extension by themselves. Firefox auto-updates only when the manifest has an
`update_url`, and there's no public web address for one yet. Once a GitHub repo exists, an `updates.json` there
can provide it.

### Store listings (not used)

Chrome-family browsers stay on **Load unpacked**; the Chrome Web Store charges a developer fee. If that ever
changes, store listings would give one-click installs and remove the Developer mode steps:

| Store | Cost | Covers | Upload |
|---|---|---|---|
| addons.mozilla.org, *listed* | free | Firefox family; public search | Same zip. A human review can take days because the extension reads cookies on all sites |
| Chrome Web Store | one-time $5 developer fee | Chrome, Brave, Vivaldi, Opera (with its "install Chrome extensions" add-on) | `FluxDM-extension-chromium.zip`; needs a privacy policy and a reason for each permission; review takes days |
| Microsoft Edge Add-ons | free | Edge | `FluxDM-extension-chromium.zip` |
| Opera add-ons | free | Opera | `FluxDM-extension-chromium.zip` |

Once a store listing exists, the "Add to …" button for those browsers should open the store page instead.
That's a small change in `ext.rs` (`install_in`).

## 5. The torrent engine patches

librqbit is a good engine but lacked things the old app had. Instead of switching to libtorrent, which would
be a much bigger job, its crates were copied into `src-tauri/vendor/` and patched. Each change carries a
`FluxDM patch` comment. What they add:

- **Trackers tab:** tier, status, seeds, leeches and message for each tracker.
- **Peers tab:** progress, per-peer speeds and flags; Seeds and Peers columns as "connected (swarm total)".
- **File priorities:** Don't download / Low / Normal / High / Maximum.
- **Read-only finished files:** other programs can open finished files while FluxDM seeds them.
- **Hole-punching (BEP 55):** reaches peers behind CGNAT/double NAT through a peer you both know.
- **Availability column:** turns red below 1.00, meaning some piece isn't on any connected peer.
- **Peer turnover:** swaps out useless peers when all connection slots are full.
- **Download in order:** `ManagedTorrent::set_sequential` makes the piece picker hand out pieces from the start of each file to the end instead of first, last, then the middle.
- **Slow limits and No upload:** the limiter's bucket holds at least two 16 KB blocks, so limits under 32 KB/s work as set, and an upload limit of 1 B/s means *answer no requests* (stock librqbit refused such a limit and ended the torrent's upload task).

The one remaining difference from the old app is that there's no upload-slot limit. The upload speed limit
still applies. For upgrading librqbit later, follow `vendor/PATCHES.md`.

## 6. Building

You need Rust (MSVC), Visual Studio 2022 Build Tools (MSVC + Windows SDK) and the Tauri CLI. Use
**PowerShell**, not Git Bash, because Git Bash's `link` command hides the MSVC linker.

```powershell
cd <the source folder>
# optional: $env:CARGO_TARGET_DIR = "D:\some\cache"   (default: src-tauri\target, several GB)
.\install.ps1          # build + install for this user
.\build-setup.ps1      # dist\FluxDM-Setup-<ver>.exe + portable exe + sha256
.\pack-extension.ps1   # extension zips for the stores
.\package-release.ps1  # dist\share\: the setup + install notes, and the source zip (section 7)
cd src-tauri; cargo test --release   # 12 tests
```

Notes:
- The UI is built into the exe, so changes in `ui/` need a rebuild.
- Builds use every CPU core. A first build takes several minutes; after that, rebuilds take a minute or two.
- If the compiler itself crashes (`STATUS_ACCESS_VIOLATION`), that's usually unstable hardware under load
  (RAM, overclock); run the build again.

## 7. Sharing it

Run `.\package-release.ps1`. It builds the setup and puts two separate things in `dist\share\`:

| Send | To whom |
|---|---|
| `FluxDM-Setup-<version>.exe` + `How to install FluxDM.txt` | anyone who just wants to use it |
| `FluxDM-<version>-source.zip` | anyone who wants to read or build the code |

The source zip holds everything needed to build (including the signed extension) and nothing generated:
no `dist\`, no `target\`. Email services often block `.exe` files, even inside a zip, so send the setup
through Discord, Google Drive, WeTransfer or similar.

## 8. Releasing an update of the app

1. Raise the version in `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
2. Push a tag `v<version>` to the GitHub repo. `.github/workflows/release.yml` builds and publishes the setup,
   the portable exe and its `.sha256`.
3. On each PC, set **Settings → Updates → Update source** to `owner/repo`. FluxDM then checks that repo,
   downloads the new exe, verifies its SHA-256 and restarts into it.

Without a GitHub repo, send the new setup instead. Running it over an existing install keeps all data.

## 9. Testing without touching your real data

Set `FLUXDM_DATA=<some folder>` to start a copy with its own data. It runs next to your normal FluxDM. Give it
a different bridge port in its `settings.json` (`LocalApiPort`, for example `17878`). Also set
`DefaultFolder` to a test folder: a test once dropped a torrent into the real Downloads folder.

## 10. Troubleshooting

| Problem | Fix |
|---|---|
| Browser downloads don't go to FluxDM | Check the extension popup: it must say **connected**. FluxDM must be running, with Settings → Browser & clipboard → *Browser bridge* on, port 7878. |
| Firefox says the add-on is corrupt | You got an unsigned build: the `signed\` file was missing when FluxDM was built. Rebuild with it in place. |
| Chrome shows "Disable developer mode extensions" | Normal for unpacked extensions. Close it with the X; the extension keeps working. Don't click "Disable". |
| Torrents are slow or have few peers | Allow FluxDM through the firewall when it asks at the first torrent start. Without that, nobody can connect to you. |
| A torrent's file is "in use" | Finished files are held read-only, so other programs can open them. To edit, move or delete one yourself, remove the torrent from FluxDM first. |
| Magnet links still open another app | Windows keeps its own default above FluxDM's registry entry. Use Settings → Torrents → *Make FluxDM the magnet handler*; it opens Default apps, where you choose FluxDM for MAGNET. |
| Magnet links keep opening FluxDM after *Give magnet links back* | FluxDM was picked in Windows' Default apps, which outranks the registry entry FluxDM restores. FluxDM opens Default apps for you: choose another app for MAGNET there. |
| A finished torrent no longer shares | Right-click → *Resume sharing*. It shares until the seed ratio; it refuses (and says why in the status) when the ratio is reached, the seeding limit is *No upload*, or files were moved or deleted (*Force recheck* fetches what's missing). |
| The window is blank (white or black) | Tray icon → *Reload window*. FluxDM also repaints a freshly shown window and nudges one that goes quiet. Details are in `src-tauri/src/webview_guard.rs`. |
| A toast pulled me out of my game | Toasts are shown without taking focus (the window is non-focusable, and if the webview still grabs focus FluxDM gives the foreground back to the previous window within about 0.7 s). Each shows for about 6 s (longer while the mouse is on it, at most 15 s); if the page ever stops answering, FluxDM hides it after 25 s. Exclusive-fullscreen games can't show any overlay. Clicking a toast opens FluxDM on purpose. |
| Set location on a half-finished torrent is slow | Unfinished torrents have full-size placeholder files. Across drives FluxDM offers to delete them and start over in the new folder instead of copying. |
| Something crashed | Check `%APPDATA%\FluxDM\error.log`. |
