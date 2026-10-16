# FluxDM 2 (Rust/Tauri) vs FluxDM 1.x (.NET/WPF) — feature parity

Audit of every feature in the .NET source (`FluxDM-source`, its `INFO.md`) against the port.
✅ = same behaviour (tested = exercised in the real app on 2026-10-01), ➕ = new or better, ⚠️ = differs, ❌ = missing.

## Downloads (HTTP)

| Feature | Status |
|---|---|
| Multi-connection segmented downloads (1–64), sparse `.fluxpart`, per-segment resume | ✅ tested (10 MB file, hash identical to a single-stream download; 8 conns ≈ 3 s vs 337 s) |
| Work stealing (finished connection takes half of the largest remaining segment) | ✅ |
| Change connection count live | ✅ |
| Resume after pause, crash, app restart | ✅ tested (quit mid-download → 8 segments + bytes restored) |
| 429/503 adaptive back-off ("server allows ~N") | ✅ tested (OVH throttled to 2) |
| Servers without range support: single stream, restart from zero | ✅ |
| Backup links (mirrors): parallel use, 32 KB start+middle content check, dead-link dropping, added live | ✅ tested (cdn + mirrors.edge kernel.org) |
| Checksums MD5/SHA-1/SHA-256/SHA-512, "Verifying n%" | ✅ tested (match → Complete, wrong hash → "Checksum mismatch" error) |
| Hugging Face X-Linked-Etag SHA-256 picked up automatically | ✅ tested |
| Refresh link (keep bytes when the size matches) | ✅ |
| Content-Disposition / URL file names, unique names `name (n).ext` | ✅ |
| Custom headers (Authorization, Cookie), cookies stripped from disk after completion | ✅ |
| Stall watchdog (0 connections for 100 s → reconnect, 3 min cooldown; torrents excluded) | ✅ |
| Not-enough-disk-space check | ✅ |
| Retries per connection, read timeout, queue auto-retry | ✅ tested (404 in a running queue → "Waiting to retry") |
| Default User-Agent `FluxDM/1.0` kept for existing installs (INFO: fake browser UAs get dropped) | ✅ (new installs say FluxDM/2.0) |

## Video (yt-dlp)

| Feature | Status |
|---|---|
| yt-dlp + ffmpeg fetched from official releases on first use, into the data folder | ✅ tested ➕ an interrupted ffmpeg download (≈100 MB) resumes instead of starting over |
| Qualities best/2160…360/MP3/M4A, playlists into their own folder | ✅ tested ("Me at the zoo": AV1 + AAC merged by ffmpeg, same `title [id].ext` naming as before) |
| Video sites auto-switch the Add window to video mode | ✅ |
| Browser cookies (Auto/Waterfox/Firefox/Chrome/Edge/Brave), Node as JS runtime | ✅ ➕ Auto falls back to Firefox when Waterfox isn't installed |
| Plain-English explanations of yt-dlp errors (bot check, age limit…) | ✅ |
| Update yt-dlp button | ✅ |

## Torrents

| Feature | Status |
|---|---|
| Magnet links, .torrent links, .torrent files; metadata cached in `torrents\` | ✅ tested (archive.org Big Buck Bunny) |
| Pick files in the Add window; change wanted files later (Content tab) | ✅ tested (only the ticked files downloaded; MD5s match archive.org) |
| File priorities Low / High / Maximum | ✅ tested: Content tab dropdown (Don't download / Low / Normal / High / Maximum), saved with the item; the patched engine fetches higher-priority files first |
| DHT with working bootstrap nodes (INFO: only transmissionbt/libtorrent answer) | ✅ librqbit's defaults are exactly those two; tested 71 nodes |
| µTP (BEP 29) | ✅ librqbit 9, TCP + µTP, honours the existing "TorrentEnableUtp" switch |
| Holepunch (BEP 55) through CGNAT | ✅ added to the patched engine (relay, initiator and target roles; PEX now also shares peers behind NAT). Tested: other clients in a live swarm answered its requests. The Peers tab shows the counts |
| Peer turnover tweak | ✅ ported to the patched engine (same rule as the .NET fork; unit-tested; it correctly leaves seeds alone in a live swarm) |
| Upload slots per torrent, global connection limit | ⚠️ not in the engine: it uploads to every interested peer, capped by the upload limit; per-torrent connection limit ✅ |
| UPnP port mapping | ✅ |
| Fast resume (no re-check after a clean quit) | ✅ librqbit fastresume + session persistence |
| Seed until ratio, separate upload limits while downloading / while seeding, "No upload" | ✅ |
| Per-torrent download/upload/seeding limits (right-click) | ✅ (a torrent re-adds itself to apply a new own limit) |
| Force recheck, force reannounce, set location, add trackers, copy name/hash/magnet, export .torrent | ✅ |
| Trackers tab: tier, seeds, leeches, message | ✅ tested: tier (numbered as before; the engine announces to all tiers at once), status, seeds, leeches, peers received, next announce, message |
| Peers tab: address, connection, flags, client, progress, speeds, downloaded, uploaded | ✅ tested: all of them (speeds from the byte counters, progress from the peer's pieces, flags incoming/outgoing · seed · wants our data) |
| Seeds / Peers / Availability columns | ✅ tested: Seeds and Peers are "connected (swarm total)" like before, e.g. `10 (209)`; Availability counts copies of what's still needed (shown red below 1.00) |
| Firewall offer the first time the engine starts | ✅ ➕ the rule now covers any port, so the random port doesn't break it |
| Magnet handler switch (HKCU, restores the previous owner) | ✅ |
| Empty placeholder files for unticked files | ➕ removed when the torrent finishes |
| Finished files usable while the torrent is loaded (seeding or paused) | ✅ tested ➕ the patched engine holds finished files read-only, so other programs can open them |

## Organising, limits, scheduler

| Feature | Status |
|---|---|
| Sidebar: All / Active / Unfinished / Finished, categories, queues (running dot), tags, counts | ✅ |
| Tags (edit, filter) | ✅ |
| Queues: max at once, own speed limit, start on launch, start/stop times, days or one date | ✅ tested (queue ran, retried a failure) |
| Post-queue action (exit/sleep/hibernate/shutdown/restart, force) with a 60 s cancellable countdown | ✅ |
| Files in queue: order up/down, remove | ✅ |
| Global limit box (presets + typed "750k", "2 MB/s"…) | ✅ |
| Speed schedule (window may cross midnight; stricter limit wins) | ✅ |
| Per-download limit (details box + right-click presets + custom) | ✅ |
| Statistics: today / 7 days / month / all time, 30-day chart, per-month table, reset | ✅ tested ➕ "time downloading" now counts only seconds with data moving |

## Window & UI

| Feature | Status |
|---|---|
| Toolbar, search (Ctrl+F), keyboard (Ctrl+N, Ctrl+V, Del, Enter, Space, arrows, Ctrl+A) | ✅ |
| Columns: show/hide, presets (Simple / +torrent / everything), resize, drag to reorder, remembered | ✅ (layout file format unchanged) |
| Sort by column | ✅ |
| Details pane: General (file map, 60 s speed graph, connections slider, limit, mirrors, checksum), torrent tabs | ✅ |
| Resizable sidebar / details / details split, remembered | ✅ |
| Window size, position, maximized remembered | ✅ (same settings fields as before) |
| Context menu (every item of the original) | ✅ |
| Delete: keep files vs Recycle Bin for finished items | ✅ |
| Drag & drop links, `.txt` lists, `.torrent` files | ✅ |
| Game covers (Steam search, strict matching), choose image, file's own icon, no icon | ✅ tested ("Portal 2") ➕ when Windows would draw its blank page (no app, or an app with a broken icon) FluxDM's category glyph shows instead |
| Light / Dark / System theme | ✅ |
| Taskbar progress, title "42% · 1.2 MB/s — FluxDM", tray tooltip | ✅ |
| Notifications (complete, failed, queue finished, started from browser); click → show in folder | ✅ tested (own toast window instead of tray balloons) ➕ shown without stealing focus and auto-hidden after 12 s (2.1.2–2.1.5, not tested live) |

## Integration

| Feature | Status |
|---|---|
| Clipboard catching (file-type list, ignores own copies and duplicates) | ✅ |
| Browser bridge 127.0.0.1:7878: `/api/add` (extension, autostart), `/api/ping`, `/api/exit`, legacy `/add` `/batch` `/show` `/ping`, web-page origins refused | ✅ tested |
| Browser extension (Waterfox/Firefox xpi, Chrome/Edge/Brave unpacked) | ✅ ➕ the xpi is signed by Mozilla (installs in regular Firefox); "Add to <browser>" buttons for every installed browser; Chromium gets its own manifest. Tested: loads in Chrome 139 and its popup shows "connected" |
| Bookmarklets "Send to FluxDM" / "Grab links" | ✅ |
| Phone remote: page, PIN, 10 wrong PINs → 5 min lockout, QR pairing page (only from the PC running FluxDM), pause/resume/remove/limit/add | ✅ tested |
| Firewall rule helpers (UAC) for the remote and torrents | ✅ |
| Start with Windows (tray), close to tray, keep PC awake | ✅ |
| `FluxDM.exe <url>` / `--tray` / `--exit`, single instance forwarding | ✅ |
| `FLUXDM_DATA` test copies run beside the real one | ✅ |
| Portable mode (`portable.txt`) | ✅ |
| error.log for crashes | ✅ (panics) |
| Corrupt data files after a power loss / PC crash set aside as `.corrupt`, app starts clean | ✅ tested (real PC crash on 2026-10-01) |

## Install & update

| Feature | Status |
|---|---|
| Needs .NET 10 Desktop Runtime | ➕ gone — one exe, WebView2 only (built into Windows 11) |
| Inno Setup installer (desktop icon, start with Windows, magnet + .torrent handlers, portable, per-user/all-users) | ⚠️ Tauri NSIS setup (`dist\FluxDM-Setup-2.0.0.exe`, 6.8 MB, tested silent `/S`): per-user into `%LOCALAPPDATA%\FluxDM`, Start Menu + desktop shortcuts, .torrent association, uninstaller in Apps; start-with-Windows and magnet handler are switches in Settings |
| Self-update | ➕ GitHub releases, SHA-256 verified, swaps the exe and restarts |
