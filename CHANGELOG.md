# Changelog

What changed for people using FluxDM. Developer notes are in `PROJECT_STATE.md`.

## 2.2.1 — 2026-10-26

### New
- **Upload limits in the Add window for torrents:** *while downloading* and *when finished*, with the same presets as the right-click menu, including *No upload*.
- **Resume sharing a finished torrent** (right-click → *Resume sharing*). *Pause* on a finished torrent is now called *Stop sharing*. Before, nothing could start sharing again, and finished torrents never shared again after FluxDM restarted. FluxDM won't resume one whose files were moved or deleted, and says why in its status.

### Fixed
- **"No upload" uploaded at 32 KB/s**, and every torrent limit under 32 KB/s (such as the 20 KB/s preset) was raised to 32 KB/s. Both now work as set.
- A finished torrent whose seeding limit is *No upload* no longer sits "seeding" forever without sending anything; it stops sharing.
- **Clear finished** left finished torrents sharing in the background, with no row left to see or stop them.
- **Updating FluxDM could leave it closed:** the new version sometimes started while the old one was still closing, handed itself over to it, and quit. It now waits for the old one to exit.
- An app update, or the first-time yt-dlp/ffmpeg download, could hang forever on a connection that went silent. They now stop with a message after 60 seconds without data.
- *Give magnet links back* did nothing when FluxDM had been chosen in Windows' Default apps; it now opens Default apps and says what to pick there.
- *Force recheck* on several torrents stopped at the first one that couldn't be checked; the rest are checked now, and every failure is listed. A finished torrent whose recheck fails stays finished instead of turning into "Paused".
- With backup links, a full disk or a folder Windows won't let FluxDM write to was blamed on the links one by one ("link dropped"); it now stops with the real reason straight away.
- The phone remote's page refused magnet links, although the PC accepts them, and a link the PC rejected failed silently.

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
