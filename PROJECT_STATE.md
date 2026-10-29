# FluxDM 2 — project state

Rust + Tauri 2 port of the .NET/WPF FluxDM (its notes are in `INFO.md` in the .NET source). Goal: Windows-only, no runtime to install, self-updating, same features and the same data
files (`%APPDATA%\FluxDM`: downloads.json, settings.json, queues.json, stats.json, PascalCase JSON).

## Layout

| Path | What |
|---|---|
| `src-tauri/src/main.rs` | Tauri setup, tray, windows (main/add/toast), every `#[tauri::command]`, startup args |
| `src-tauri/src/manager.rs` | list, runs, queues, scheduler, limits, stall watchdog, 500 ms tick, list diffs to the UI |
| `src-tauri/src/http_dl.rs` | multi-connection HTTP engine (segments, work stealing, mirrors, 429 back-off, checksum) |
| `src-tauri/src/video.rs` | yt-dlp/ffmpeg install + runner |
| `src-tauri/src/torrent.rs` | librqbit 9 session (TCP+µTP, DHT, UPnP, fastresume), metadata, seeding/ratio |
| `src-tauri/vendor/` | patched librqbit 9.0.1 crates (`[patch.crates-io]`); every change is listed in `vendor/PATCHES.md` |
| `src-tauri/src/api.rs` | browser bridge (127.0.0.1:7878) + phone remote (LAN, PIN, lockout, QR page) |
| `src-tauri/src/covers.rs` | Steam cover lookup, custom images, Windows file icons via `img://` protocol |
| `src-tauri/src/win.rs` | Win32: sparse files, keep-awake, recycle bin, clipboard, magnet handler, firewall, shell icons |
| `src-tauri/src/webview_guard.rs` + `ui/guard.js` | keeps WebView2 from going blank: browser flags via env var, repaint nudge on show, page heartbeat watchdog (nudge after 15 s quiet, reload after 90 s quiet and focused), tray *Reload window* |
| `src-tauri/src/update.rs` | GitHub-release self-update (FluxDM.exe + .sha256, rename-swap) |
| `src-tauri/extension/` | the browser extension, embedded in the exe (`ext.rs`); `signed/FluxDM-extension.xpi` is Mozilla's signed copy, embedded when present (`build.rs`) |
| `pack-extension.ps1` | packs the extension for addons.mozilla.org and the Chromium stores into `dist\` |
| `ui/` | plain HTML/CSS/JS: `index.html`+`main.js` (main window), `add.*`, `toast.*`, `common.js`, `style.css` |
| `install.ps1` | build + install for this user to `%LOCALAPPDATA%\FluxDM` |
| `build-setup.ps1` | NSIS setup + portable exe + sha256 into `dist\` |
| `package-release.ps1` | `dist\share\`: setup + `How to install FluxDM.txt` (for users), source zip (for developers) |
| `.github/workflows/release.yml` | tag `v*` → build + GitHub release |

## Build notes

- PowerShell, not Git Bash (its `link` shadows the MSVC linker). `CARGO_TARGET_DIR` may point anywhere; the
  scripts default to `src-tauri\target`.
- Needs VS 2022 Build Tools (MSVC + Windows 11 SDK) and, for the setup, the Tauri CLI.
- Settings keeps unknown JSON fields (`#[serde(flatten)] extra`), so the .NET-only ones survive a save
  (window geometry, TorrentEnableUtp, TorrentMaxConnectionsPerTorrent… — some are read from `extra`).

## Testing

- `FLUXDM_DATA=<dir>` gives a test copy its own data folder; such a copy skips the single-instance lock and runs
  beside the real one. Give it its own `LocalApiPort` (and `RemotePort`) and a `DefaultFolder` that isn't the
  user's Downloads (INFO.md: a UI test once wrote a torrent into the real Downloads).
- `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333` lets a script drive the real windows over
  the DevTools protocol (Runtime.evaluate / Page.captureScreenshot). `webview_guard::apply_env` appends its own flags to
  whatever is already set, so this still works. To confirm the guard flags are live, look at the command line of any `msedgewebview2.exe` of the app.
- Run the *installed* exe (`%LOCALAPPDATA%\FluxDM\FluxDM.exe`) for torrent/remote tests, once its firewall rules
  exist (the app offers them on first torrent/remote start; older rules for `…\Programs\FluxDM` are stale).
- UI previews without the app: headless Chrome + a mocked `window.__TAURI__`.

## Status (2026-10-07)

- Version 2.2.1. It takes over the .NET FluxDM 1.0's data as-is; uninstalling 1.0 keeps `%APPDATA%\FluxDM`.
- `cargo test`: 12 passed (at 2.0.1; the later changes were only compiled, not run live). Feature-by-feature results are in `FEATURE_PARITY.md`.
- The torrent engine is a patched librqbit 9.0.1 in `src-tauri/vendor/`. It adds tracker status, peer piece
  counts, file priorities, read-only finished files, BEP 55 holepunch, PEX with flags, availability, peer turnover and sequential download. See `vendor/PATCHES.md`.
- Still different from .NET: no upload-slot limit (the engine uploads to every interested peer, within the
  upload speed limit).

### Unreleased fixes (2026-10-29)

- Desktop dialogs, Settings and theme: accessible modal headers, top-dialog Tab trapping, protected form backdrops and wrapping actions; clearer removal/action labels and safe formatted update notes. Settings uses aligned section groups, Advanced fields and shared speed controls, also in Scheduler. Theme events update main/Add/toast pages; native frames follow saved themes at startup, creation and save. `node --check ui/common.js`, `node --check ui/main.js` and `node --test tests/ui-regressions.cjs` pass (15 tests). `cargo build`, `cargo test` and `cargo clippy` with `--offline --manifest-path src-tauri/Cargo.toml` pass (42 tests, one ignored; 12 existing warnings in untouched files), with test data and webview folders under target. Live native title bars and open-window transitions remain unverified; an isolated headless preview could not start its renderer.

- Small-window desktop layout: toolbar labels yield at 1340/1100px, More at 760px or whenever actions do not fit, and expandable search at 900px. Details hide below 560px height without overwriting saved dimensions; General stacks below 720px of content width, Scheduler at 900px and Add below 600px. Main minimum raised to 640x420. JavaScript syntax, 11 UI regressions, offline build and isolated headless layout/interaction checks pass (88 toolbar/footer/height combinations plus form and saved-size checks); live app not run.

- Desktop UI polish improves muted contrast, form controls, button states, empty lists and tracker status badges. JavaScript syntax and UI regression checks run; live visual testing not performed.

- Window recovery separates page heartbeats, repaint attempts and grace periods; input idle, desktop lock and display-off defer recovery. The page keeps one animation-frame request pending.
- Queue completion uses the non-empty set of IDs started in the current run. Removed IDs are ignored; paused/failed members block completion. Starting resets membership, editing preserves it, and update resume carries it forward. Countdowns retain their force choice.
- Torrent duplicates are checked across statuses, batches and resolved hashes; existing rows retain ownership of engine limits and selection. Moves preflight conflicts and use exclusive copies with rollback before deleting originals.
- Checksum verification preserves partial data and progress until success. VerificationPending survives pause/startup and resumes locally, including non-resumable files.
- Update resume is a one-use snapshot of active items, queue run IDs and sharing torrents. Invalid records fall back to launch queues; pre-update pause is bounded and spawn failure resumes the snapshot with services intact.
- HTTP validators prefer strong ETag over Last-Modified and belong to their exact source URL and type. Changed validators or full responses to validated ranges stop workers before discarding partial data; mirror validators and legacy unscoped partials preserve progress.
- Settings update checks use the entered repository without saving. Modal keys follow inner controls, affect the top dialog and restore focus safely.
- Download budgets span HTTP, torrents and video processes. Each 500 ms tick allocates max-min fair shares under global, queue and item caps, returning idle capacity with measured-rate +25% headroom (minimum 4 KB/s). Busy fixed-rate videos remain eligible for growth; yt-dlp reconnects only for changes over 30%, after at least 60 seconds per run. Limited videos use one fragment worker; seeding consumes no download share.
- Torrent upload budgets are separate aggregates for downloading and seeding, shared equally and tightened by item caps. No upload affects only its phase/item; session caps stay cleared.
- Add forms merge requests only with identical headers, video flag and queue, and no forced filename on either request. Window labels retain context and pending links through loading; distinct contexts remain isolated. Torrent probes/resolution retain supplied headers; redirects forward custom headers only to the original exact origin.
- The extension tracks POST frame-request chains before the downloads API fires, including redirects and worker suspension. Stored URL exclusions last 30 minutes and leave matching downloads in the browser.
- VideoWorkFolder and VideoFiles record ownership. New jobs stage output/partials/fragments privately in the selected destination, then publish without replacement. Removal/restart waits for process exit and cleans owned files; completed Keep files preserves outputs, recycling handles each output, and cleanup failure retains the row. Legacy untracked artifacts require manual cleanup. Playlists total distinct output sizes and target their directory.
- Seeding ticks refresh upload/peer/swarm data and explicit runtime seeding state. Inactive details use relevant-data fingerprints, retain refreshes requested in flight, and cache torrent metadata by path/length/modification time.
- Offline build, unit, lint and UI regression checks cover these changes. Live browser interception, transfer rates, swarms, update handoff, window transitions, video processes and recycling remain unverified.

### Changes since 2.0.0 (all built, none tested live)

- **2.0.1:** audit fixes. Concurrent saves can't corrupt downloads.json (static locks in `store::write` and `Manager::save`). Tray tooltip/taskbar progress only update on change. yt-dlp probe times out after 90 s. Add window can't double-submit. Time-left column sorts by remaining time. Windows reserved file names (con, COM1…) get a `_` prefix. The extension's `/api/add` returns 400 when nothing was downloadable. Queue start/stop times work across midnight (a 23:58 start is still honoured at 00:01, with the right weekday and once-date).
- **2.0.2:** magnet handler. Windows' own default (set in Settings, e.g. by qBittorrent's installer) outranks `HKCU\Software\Classes\magnet`, so `win::is_magnet_handler` now asks `AssocQueryStringW` what really launches. FluxDM also registers `Software\FluxDM\Capabilities` + `RegisteredApplications` so it is listed in Default apps, and when turning the handler on does not win it opens `ms-settings:defaultapps?registrypathtosearch=...` (through `cmd /c start`; `explorer <uri>` opens Documents instead).
- **2.1.0:** *Download in order* for torrents (`Item.sequential`, patched engine, set each tick in `torrent::tick`). *Set location* on an unfinished torrent moving to another drive no longer copies the full-size placeholder files: the UI asks, then the old partial files are deleted and the torrent starts over in the new folder (`torrent_move(restart_if_unfinished)`, `torrent::same_drive`). Same-drive moves are still renames.
- **2.1.1:** blank white/black window protection (`webview_guard.rs`). `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` is set at startup for every window (`CalculateNativeWinOcclusion` off, background/renderer throttling off; wry's default disable-features kept); a 1 px resize after a hidden window is shown; the main page sends a heartbeat only while `requestAnimationFrame` is firing, and a visible window that goes quiet is nudged then reloaded. Main and Add windows start with a dark background colour (main in `tauri.conf.json`, Add from the system theme). Research (2026-10-07): there is no official Microsoft fix. Sources: WebView2Feedback #2983 and #1077 (blank when hidden/background), tauri-apps/tauri #2496 (black after show, needs a redraw) and #13092 (custom per-window browser args blank other windows, hence the env var), iLuzionsX/mouser PR #5 (the renderer-backgrounding flags). Not used: `--disable-gpu` (last resort, costs smoothness).
- **2.1.2:** toasts no longer steal focus (they tabbed the user out of a game). `win::show_noactivate` shows the toast window with `SW_SHOWNOACTIVATE`, sets `WS_EX_NOACTIVATE` (not `WS_EX_TOOLWINDOW`: it made a caption bar appear, fixed in 2.1.3) and keeps it topmost without activating. Clicking a toast still opens FluxDM on purpose. Not tested live; exclusive-fullscreen games can't show any overlay.
- **2.1.3 / 2.1.4:** 2.1.3 dropped `WS_EX_TOOLWINDOW` from the toast (it showed a title bar). 2.1.4 adds a Rust-side hide 12 s after a toast is shown (`show_toast`), because the page timer did not always dismiss it.
- **2.1.5:** the toast is shown with plain `w.show()` again. `win::show_noactivate` (2.1.2) bypassed tao's show ordering and made a full title bar appear. tao already sets `WS_EX_NOACTIVATE` (window is `focusable(false)`) and shows with `SW_SHOWNOACTIVATE`; since the webview can still take focus, `show_toast` remembers the foreground window and gives it back if the toast grabbed it (`win::give_back_foreground`, checked at 60/250/700 ms).
- **2.2.0 (2026-10-16; built and installed, release v2.2.0; the new UI was checked in a headless preview with a fake backend, not clicked through live):**
  - **Toasts that never closed / a missing "Download complete".** Root cause: since 2.1.4 the page and Rust each hid the window on their own clock. When Rust's 12 s backstop hid it (page timer stalled, or the mouse had been on the card so `mouseleave` never came), the page still thought a card was up: every later toast went into its queue and was never drawn, while Rust kept showing the window with the stale card (often the "failed" one) for 12 s at a time. Toasts could also reach the page out of order (one spawned task per toast) and leave the page's `gen` behind Rust's, so its hide was ignored. Now the page owns the screen: Rust only forwards toasts in order (`TOAST_INBOX`, held until the page calls `toast_ready`), the page calls `toast_show(gen)` / `toast_hide(gen)`, a hide only counts for the card that is up (`TOAST_SHOWN`), hover holds a card for at most 15 s, and Rust's 25 s backstop also sends `toast-reset` so the page starts over. Checked with a simulation of the page logic, not kept in the repo (lost mouseleave, stalled timer, failed + queue-finished burst, started then complete): all pass.
  - **Add window:** the link is one line (▾ opens every line, copy button, "N links" badge for a batch; Enter starts, Shift+Enter adds a line). Torrents get a qBittorrent-style file tree (folders with tri-state boxes, sizes, filter, All/None on the filtered files) that fills the window, which grows once to about 760×800. Connections, file name, checksum and the advanced section are hidden for torrents; checksum moved into Advanced.
  - **Speed limits:** one combo for the global box, the Add window and the details panel (`speedCombo` in `common.js`): type any speed or pick a preset. Presets now 64, 128, 192, 256, 384, 512, 768 KB/s, 1, 1.5, 2, 3, 4, 5, 7.5, 10, 15, 20, 25, 50 MB/s; upload presets got the same in-between steps.
  - **Open when it finishes** (`Item.open_when_done`, JSON `OpenWhenDone`): a box in the Add window, the details panel and the row menu. `on_ended` opens the file (or the torrent's folder) with `win::open`.
  - **Public repo:** https://github.com/MohammadFakih02/fluxdm (MIT; vendored librqbit stays Apache-2.0). The update source defaults to it (an empty `UpdateRepo` is filled at start-up). Added `CHANGELOG.md`, `CLAUDE.md`, `LICENSE`, CI (`.github/workflows/ci.yml`) and the docs-rule hook `.githooks/pre-commit`: every code commit must also update `PROJECT_STATE.md` and `CHANGELOG.md`.
  - **Browser extension 1.4.0:** *Catch every file type* (on by default): any real download goes to FluxDM, not just the listed types. The list (now much longer) is used when that's off and by *Grab download links*. The app's clipboard list got the same longer default; anyone still on the old default is moved to it at start-up. The Firefox `.xpi` in `extension/signed/` is still 1.3.0 until it is re-signed on addons.mozilla.org (`pack-extension.ps1`).

- **Releases (2026-10-16):** `release.yml` now names the release "FluxDM vX.Y.Z" and uses that version's `CHANGELOG.md` section as the notes (plus a download/SmartScreen line), like Deck's releases. Write the CHANGELOG section before tagging.
- **2.2.1 (2026-10-26) — code review and bug fixes; built, tested with an isolated test copy (`FLUXDM_DATA`, own ports, own `WEBVIEW2_USER_DATA_FOLDER`) driven over DevTools:**
  - **Torrent upload limits in the Add window** (requested): *while downloading* / *when finished* speed boxes, torrent mode only (`[data-torrentonly]`), sent as `uploadLimitKbps` / `seedingLimitKbps` in `AddSubmit` and stored on the item. `UPLOAD_PRESETS` moved to `common.js`; `speedCombo` takes `allowNone` (-1 = No upload). Tested: picked *No upload* from the ▾ menu and typed `20k`; `downloads.json` had `UploadLimitKBps -1`, `SeedingLimitKBps 20`.
  - **No upload / slow limits:** engine patch in `vendor/librqbit/src/limits.rs` (see `vendor/PATCHES.md`): bucket of at least 32 KB, and 1 B/s = upload blocked (requests dropped). `torrent.rs` sends `NO_UPLOAD` (1 B/s) for -1 instead of `rate(1)`, which `MIN_RATE` had raised to 32 KB/s; `MIN_RATE` is now 4 KB/s. Engine tests: 36 pass, including the new `e2e_no_upload` (fails on the unpatched engine). Not tested against a real swarm.
  - **Seeding:** `Manager::start` on a finished torrent calls `Service::resume_seeding` (checks ratio, seeding limit, and that every wanted non-padding file exists at full size; `Meta.padding` added). `Service::tick` stops a finished torrent that is Live but not finished (data changed on disk) and one whose own or global seeding limit is *No upload*; `go()` doesn't start seeding then either. UI: *Resume sharing* / *Stop sharing* labels; `act.resume` includes finished torrents. Tested: stop → *Complete*, resume → *seeding*, seeding limit -1 → *not shared (seeding limit is No upload)*, file moved away → *can't share…*, nothing re-downloaded.
  - **Clear finished** is async and does what *Remove* does for finished torrents (engine delete without data, unused cached .torrent deleted). Tested: `session.json` no longer lists the torrent, the cached .torrent is gone, the file stays.
  - **Updater:** `install_update` starts the new exe with `--updated=<old pid>`; `main` waits for that process (`win::wait_for_exit`, max 20 s) before the single-instance plugin runs. Before, a new copy that started while the old one still had its single-instance window sent its arguments there and called `exit(0)` (plugin source checked). Tested: with a live dummy PID the new copy waited 6.6 s for a 6 s process; with an ended PID 0.2 s. The full update flow (GitHub release) was not run.
  - **Stalls:** `update::install` and `video::fetch` time out after 60 s without data (send and every chunk). Not tested live.
  - **Magnet handler:** `magnet_toggle` opens Default apps when turning *off* leaves FluxDM in charge (`needsSettings`, `turningOn`); with no backup, turning off deletes the `magnet` class if it points at FluxDM. Not tested live (it would change this PC's real association).
  - **Recheck:** `torrent_recheck` goes on after a failure and returns every error; a failed finished torrent stays *Completed*; `completed_at` is set when a recheck completes one. Tested with one torrent without info and one with data: the error named only the first, the second was rechecked to *Complete*.
  - **HTTP:** `http_dl::local_fault` (disk full / fatal I/O) ends the download straight away instead of marking each mirror dead. Not tested live.
  - **Phone remote page:** accepts `magnet:?` links (the API already did) and shows an alert when adding fails. Only the link filter was tested.
  - `cargo test`: 16 pass. Clippy: only the existing style warnings.
- **Open source (2026-10-16):** `LICENSE` is plain MIT again so GitHub detects it; the librqbit/Apache note, yt-dlp and FFmpeg moved to `THIRD_PARTY_NOTICES.md`. Added `CONTRIBUTING.md` and issue forms (`.github/ISSUE_TEMPLATE/`).
