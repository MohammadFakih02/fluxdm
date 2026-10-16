# Changelog

What changed for people using FluxDM. Developer notes are in `PROJECT_STATE.md`.

## 2.2.0 — 2026-10-16

### New
- **Torrent file picker, qBittorrent-style.** The Add window's file list is now the main part of the window: folders you can open and close, a box per folder that ticks everything inside, sizes, and a filter. *All* / *None* work on the files the filter shows.
- **One-line link box** in the Add window, with a ▾ to see every line and a copy button. A batch shows how many links it has. Enter starts the download; Shift+Enter adds another line.
- **Speed limits you can pick or type** in the Add window and in the details panel, like the global limit. More presets, with finer steps at the low end (64, 128, 192, 256, 384, 512, 768 KB/s, 1, 1.5, 2, 3, 4, 5, 7.5 MB/s and up).
- **Open when it finishes:** tick it in the Add window, in the details panel, or right-click a download.
- **Browser extension 1.4.0: catch every file type** (on by default). The file-type list is much longer and is only used when this is off, and by *Grab download links*.
- Updates are checked against this repository by default.

### Changed
- Torrents don't show connections, a file name or a checksum in the Add window. The checksum box moved into *Advanced* for normal downloads.

### Fixed
- Notifications that stayed on screen, or showed an old message, after a failed download.
- A "Download started" notification that was never followed by "Download complete".

## 2.1.6 and earlier

See the release notes on GitHub and `PROJECT_STATE.md`.
