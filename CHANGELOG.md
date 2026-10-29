# Changelog

What changed for people using FluxDM. Developer notes are in `PROJECT_STATE.md`.

## Unreleased

### Fixed
- Dialogs keep keyboard focus inside, offer a close button, protect forms from backdrop clicks and wrap footer actions.
- Removal uses one action with an optional Recycle Bin checkbox; prompts and countdowns name their actions, and update notes use readable formatting.
- Settings groups align controls, simplify wording and share speed presets with Scheduler. Theme changes reach open windows and native title bars.
- Small desktop windows keep toolbar actions in More, collapse details when short, and stack cramped General, Scheduler and Add layouts; the main minimum is now 640x420.
- Frozen windows can recover after a repaint attempt. Recovery waits while you are away, the session is locked or the display is off, and gives the window time to recover when you return.
- Queue completion and shutdown/sleep countdowns depend only on downloads started during that queue run. Older failed or paused entries do not block completion; paused or failed downloads from the current run still do, and removed downloads are ignored. Each countdown keeps its captured force setting.
- Adding the same torrent again keeps the existing download and shows its name and status, including when adding several links or resolving magnet metadata.
- Pausing a checksum check keeps the download paused. Resuming checks the saved file again before marking it complete, notifying you or opening it.
- Updates resume the previous downloads and queues once. If the updated version cannot start, FluxDM stays open and resumes downloading.
- “Check now” in Settings checks the entered repository without saving other changes in the form.
- Enter and Escape affect only the top dialog after its controls have handled their own keys. Enter chooses the intended primary button, and closing a dialog restores the previous focus.
- HTTP downloads restart safely when the source file changes, while different mirror identifiers preserve progress.
- Moving torrent files refuses existing destination files. If a copy fails, partial copies are removed and the original location is kept.
- Global and queue speed budgets share spare bandwidth fairly across downloads, torrents and videos. Videos reconnect only for substantial speed changes, at most once a minute.
- Torrent upload limits apply separately to downloading and seeding torrents. Disabling uploads while downloading no longer blocks finished torrents from sharing, and seeding caps still apply during other downloads.
- Incoming links merge into matching Add forms; requests with different headers, filenames, queues or video choices stay separate, including while forms load.
- Authenticated .torrent links keep their browser request headers when fetching metadata. Redirects to a different origin receive none of those custom headers.
- The browser extension leaves POST downloads and their redirects in the browser.
- Removing or restarting new video jobs cleans their partial files and fragments. Finished outputs follow Keep files or Recycle Bin choices; cleanup failures remain available to retry. Older untracked partials need manual cleanup.
- Finished playlists show the combined file size, and Open / Show in folder target the playlist folder.
- Seeding torrents show their upload speed and peers in the list, and the footer shows the seeding count and upload speed.
- Inactive torrent details stop polling and redrawing until relevant data changes. Selection and content edits still refresh, including while an earlier request is in flight. Parsed torrent metadata is cached while its file stays unchanged.

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
