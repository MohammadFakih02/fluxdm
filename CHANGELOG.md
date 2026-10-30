# Changelog

What changed for people using FluxDM. Developer notes are in `PROJECT_STATE.md`.

## 3.0.0 — 2026-10-31

### Appearance
- Five app icon themes: Full heat (the original), Ice, Neon, Emerald and Mono, shown as icon previews. They change the tray icon, the taskbar/window icons and the logo inside the app, keeping the original artwork's light and shade. Colour swatches show a smooth ring when chosen.
- Selected download art strength (25-400%) and blur for the highlighted card in the list (applied to game banners too). The card's text follows the art behind it: a soft shadow on medium art, dark text with a light halo on bright art, so it stays readable at any strength.
- The column headers of the details tables (connections, peers, files…) are soft glass with rounded ends instead of a hard black strip.
- Header banner strength (25-150%) and blur for the big picture above the details.
- Background art strength (25-500%) and blur (Automatic, None to Very strong), so more of the game art can show behind the window.
- The Theme choice under Behaviour was removed: it had no effect (the app is always dark; Appearance sets its look).

### Artwork
- Every banner, from every source, is checked before it's used: blank padding is cropped off (e.g. a square cover on a white share image), and pictures that aren't wide and big enough are turned down, so the header shows the blurred icon or cover instead of a boxed, square or tiny picture. Existing items are re-checked once.
- Game icons come first from the PlayStation store (512 px square icons) and the Microsoft Store (square box art); only without those is the source's own icon used, and tiny ones (e.g. Steam's 32 px) are replaced by the sharper cover art. Wide or tall pictures used as icons are framed around their detail (the title and faces) instead of the plain middle; when that's wider than a square, the icon zooms out to show all of it over a blurred copy of the picture.
- In Add download, a pasted link shows from its start, and a long link no longer runs under the link icon.
- More free sources with proper wide pictures: Cinemeta backdrops for films and series, TVmaze backgrounds for series, AniList and Kitsu for anime, then Wikipedia and iTunes as fallbacks.
- Soundtracks, demos, artbooks and similar side releases are no longer taken as the art for a game. Console tags glued to other words (DLCs_PS5) now mark a download as a game.
- Right-click → Banner → Choose banner image… uses any picture from your PC (blank borders are cropped); "Find a banner automatically again" undoes it. A chosen banner is kept through automatic lookups.

### Fixed (torrents)
- If the saved DHT port is in use by another program (or a second FluxDM using a copy of the data folder), DHT starts again on a free port instead of every torrent failing; if that fails too, torrents run on trackers alone with a notice.
- Long error messages in the details header are cut to two lines; hover shows the whole text.
- Pausing a torrent works at once, even while the engine is starting or the torrent is still checking its files (it used to wait without limit).
### Discord notifications
- Settings → Discord notifications: paste a channel webhook link and choose any of five messages: a download finished, a download failed, a queue finished, the disk is almost full (a free-space level you set, at most one message per drive per hour) and a daily summary at a time you set (skipped when nothing happened). All are off by default. "Send test message" says exactly what happened. Messages are sent in the background and wait out Discord's rate limit; a failure never slows a download.

### Videos and audio
- New qualities: Smallest file, Opus and FLAC audio, and a custom yt-dlp format (Settings → Videos and audio).
- Subtitles can be downloaded as files or put inside the video, in the languages you choose (default English and Arabic), optionally with auto-generated captions.
- The thumbnail, title/artist details and chapters are added inside the file (can be turned off).
- SponsorBlock for YouTube: mark sponsor segments as chapters, or cut them out.
- A cookies.txt file can be used instead of browser cookies.
- The Add window lists a playlist's videos so you can tick which ones to download (All, None, or a range such as 1-10,15).
- Download only part of a video: type start-end (e.g. 1:30-2:45) in the Add window.
- Live streams: the Add window says when a link is live or scheduled; options to record from the start and to wait for a scheduled stream. The list shows "waiting for the stream to start", "live - recording" and "finishing the file".
- Optional "Don't download the same video twice" remembers downloaded videos; the Add window also warns when the same video is already in the list.

### Fixed (videos and links)
- The app icon has a small margin like other Windows icons and the F sits in the middle, at every size (the tray and taskbar use the largest one).
- Selecting another download no longer makes the window, the banner or the list blink: pictures are decoded before they're swapped in, measured banner sizes are reused, and known art colours apply in the same frame, new art keeps the current look until it is ready, and cards aren't rebuilt on selection.
- Dark banners (night scenes, horror games) and pictures that can't be measured now show behind the window instead of staying near-black.
- yt-dlp and ffmpeg now stop with FluxDM even when FluxDM crashes or is ended from Task Manager, instead of carrying on in the background.
- Videos with Arabic, emoji or other non-Latin titles or folders finish and appear in the list correctly, even when yt-dlp reports the path in a different encoding.
- Reading a video page no longer fails when yt-dlp prints a notice next to its answer.
- yt-dlp and ffmpeg are checked against their published checksums before they are installed, and yt-dlp must start successfully; otherwise nothing is installed and the reason is shown.
- Pasted links lose invisible characters, wrapping quotes and brackets; several links on one line are all added.
### Torrent settings
- Added an optional watched folder: stable .torrent files use the default torrent destination/category, retain cached metadata and move into an added subfolder. Invalid files remain in place with a warning; duplicate torrents are not added twice.
- Added an optional PeerGuardian IP blocklist from an HTTP(S) URL or local file, with gzip support and weekly refreshes. Failed or empty refreshes retain the last good ranges for new peer connections; initial load failures report an engine error.
- Added saved DHT, local peer discovery and IPv4-only switches. Defaults preserve existing networking; changes to these switches and the blocklist apply after restart.
- Added an optional seeding time limit in minutes, alongside the ratio target, with Stop sharing or Remove torrent (keep files). Sharing time survives restarts and excludes paused/checking time and long suspend gaps.
- New torrents and magnets can receive a saved global tracker list. Add previews and deferred/bulk additions untick configurable filename patterns, with manual file choices taking priority.
- New torrents can mark unfinished files with .!fx and remove the marker as each file completes. Saved paths survive pause, restart and moves; filename filters use original names. Conflicting marker names are rejected, and publication never replaces another file.

### Download settings
- Added None/System/HTTP/SOCKS5 proxies, optional masked login and local-address bypass for downloads, metadata and covers. SOCKS5 supports torrent TCP peers with uTP disabled; Settings explains direct DHT/UDP traffic and restart requirements.
- Regular file collisions can Ask, Add a number (default), Overwrite or Skip, preserving existing files and partial data until a choice is made.
- New HTTP and torrent downloads can use an optional unfinished folder, with saved staging paths and completion moves to their final destination.
- Speed schedules now select weekdays, including overnight windows anchored to their starting day.

### Fixed (download storage and queue safety)
- Torrent filenames now use shared Windows-safe paths for storage, Files, missing-file checks, moves and deletion. Invalid characters, reserved names, trailing dots/spaces and oversized components are sanitized; names that clash after cleaning (or differ only in upper/lower case) get " (2)", " (3)"… instead of the torrent being rejected.
- HTTP and torrent disk-full write errors pause with free-space guidance and retain partial data without automatic queue retries. HTTP paused rows now show their reason.
- Force rechecks run one at a time across torrents, support cancellation while waiting or checking, report engine failures and time out after 30 minutes. A cancelled or timed-out check retains its slot until hashing stops.
- Queue finish actions skip other active or waiting downloads and check again when the countdown ends. Failed batches report their failure count once without running an action.
- Editing a future queue start re-arms today without replaying yesterday's slot. Renaming preserves the shared speed limiter and rejects duplicate names; invalid saves leave the name unchanged.

### Scheduler and queue controls
- Scheduler now groups Name, When, Limits, finish actions and Items in order, with a plain schedule summary, weekday/weekend shortcuts, one-off dates and next-start countdowns beside queue status and counts.
- Queue items support move up/down/top/bottom, drag reordering, removal and confirmed clearing while keeping downloads and files. Live Start/Stop controls refresh without replacing edits; failed saves recover the saved form.
- Scheduled queues catch up during their current time window, including overnight and one-off schedules. Local-date guards prevent repeated starts during clock changes; a missed stop is honoured after sleep.
- Queues can stay enabled when empty and automatically start later additions. Completion notifications fire once per batch, and repeated Start requests preserve the current batch.
- Each queue has its own retry count and delay, off for new queues. Existing saved retry preferences carry over.
- Manual downloads share a configurable concurrency limit, defaulting to three across HTTP, video and torrents. Waiting downloads can be paused.
- Add remembers the last chosen queue, including None, while explicit incoming queue choices take priority. Remembered choices follow queue renames and clear when a queue is deleted.

### Fixed (torrent pause and missing files)
- Pause, Resume, Force recheck and location changes run one operation at a time per torrent. Resume requests made during another operation show a retry note; pausing interrupts magnet resolution and disk checks, including checks waiting for a slot.
- Pause waits for the previous transfer tasks to release their sockets and callbacks before resuming. Disk checking shows Checking progress and offers Pause, while cancelled resolution returns to Paused without stale Connecting text.
- Starting a saved torrent with missing or truncated wanted files preserves its progress and asks for Force recheck. Remembered engine torrents no longer open or recreate missing files before this check; unticked files do not block resuming.

### Fixed (torrent file selection)
- Unticked torrent files are no longer created for shared boundary pieces. Their bytes stay in a sparse file under `.fluxdm.parts`, which also supplies uploads of those pieces. Selecting a file later restores its retained bytes; existing files moved out of the selection keep their data there.
- Rechecks, restarts and location changes retain boundary data. Removing a torrent deletes its own part-file, including with Keep files, without affecting other torrents sharing the folder.

### Fixed (torrent checks and resume data)
- Force reannounce asks trackers and peer discovery immediately without pausing, replacing the torrent or checking files again. Paused torrents show Start it first.
- Initial checks skip pieces belonging only to unticked files and count selected boundary pieces in checking progress, reading retained bytes from part-file storage.
- Pausing, finishing and exiting save resume data, including small transfers; downloads also save every minute. Unchanged moves and tracker additions retain verified progress, while Force recheck still checks the whole selection. Failed resume hashes trigger a fresh check, and a pause during checking stays paused.

### Fixed (HTTP progress)
- Changed files, lost resume support and expired links pause with a clear explanation while keeping partial data. Only Restart from zero discards existing progress.
- HTML login and expiry pages are rejected before writing file downloads, including mirrors and single-connection transfers; intentional HTML downloads remain supported.
- Ignored byte ranges retry with backoff and fall back to one connection only before any bytes are downloaded. Servers that close ranged probes are retried without Range.

### Fixed (art fit)
- "Start with Windows" stays on after an update or reinstall: FluxDM restores its startup entry when the setting was on, and recognises its own exe however the path is spelled (short 8.3 names, different case).
- Notifications: app messages such as "FluxDM is still running" show the FluxDM icon instead of a success check, have no second frame around the card, and hovering freezes the timer bar where it is instead of restarting it. Problems (port in use, firewall, a video that couldn't be removed) show an amber warning; failed downloads show a warning sign unless they are retrying a lost connection.
- Large banners fill the header and list card cropped around the picture's subject, with a soft fade instead of a hard edge; wide windows no longer zoom in.

### Added
- New app icon: a leaning gold F with speed trails on a graphite tile that glows hot where it is heading. Drawn separately for each size, so the taskbar, tray and title bar icons stay sharp.
- While "Start with Windows" is off, FluxDM asks once every two weeks whether to turn it on ("Not now" waits another two weeks; turning it off in Settings also resets the wait).
- Seamless title bar like Windows 11 Settings in the main and Add windows: the app's own background runs to the top, search sits in the middle of the bar, and the window buttons keep snap layouts (tauri-plugin-decorum).
- Torrent Files tab is a folder tree: a top row for the whole torrent and collapsible folders with combined size, progress, priority ("Mixed" when they differ) and remaining; checking or setting a folder applies to everything in it. Files and Trackers use the full width of the details pane.
- Game artwork is keyless: Steam library heroes, Microsoft Store SuperHeroArt, PlayStation key art by ID or exact name, GOG wide covers, then smaller Steam and web images.
- Programs and ISO images can use Store heroes, vendor share images, GitHub repository art and referrer/host images, with Wikipedia, Snapcraft and Flathub as fallbacks. Screenshots stay dim; publisher and distro colours tint icon washes.
- Optional list grouping by date added, status, type/category, site or queue, saved alongside sorting; existing and new lists start ungrouped.
- Torrent Options has separate per-download pickers for download, upload while downloading and seeding. Item menus collect their presets under Bandwidth limit.
- The navigation rail can be dragged wider to show labels and counts, remembers its width, and toggles between icons and labels when its edge is double-clicked.
- Redesigned Add window with verified Steam artwork, type banners, file selection and priorities, destination free-space feedback, queue and speed controls, and a persistent action footer. Advanced keeps video quality, playlists, checksums, mirrors and request headers available.
- Redesigned notifications with completion artwork and metadata, Open and Show in folder, retry and pause actions, torrent sharing controls, and queue countdown actions synchronized with the existing confirmation dialog. Notification ordering, hover holds and click-to-reveal remain available.
- Settings now has searchable section pages, an icon rail and live Appearance controls for backgrounds, artwork accents, glass, line glow and game art. Cancel restores the preview; unsaved edits ask before being discarded.
- Details banners show game artwork or type colours, progress reveal with adjustable glow, quick actions, six live statistics and source information. Connections, Options, Mirrors and Log join torrent Peers, Trackers, Files and verified Pieces views; speed graphs include torrent uploads and file maps remain visible beside the tabs.
- Main window: seamless icon rail, resizable download cards and a full-height details column; selected banner backgrounds, type artwork and optional colours sampled from game art.
- Saved sorting with date groups, queue and tag filters, an optional column table, and footer disk space plus download/upload limit controls.
- Desktop appearance foundation: saved background, accent, glass, progress glow and game-art preferences, shared dark controls and status pills, a bundled Inter font and consistent outline icons with a gear for Settings.
- Steam matches cache a wide library banner with capsule and store-header fallbacks alongside their existing icons and covers.

### Fixed
- Files, Peers, Trackers and Connections table headers stay above scrolled rows with solid backgrounds and their own bottom borders. Files has a select-all checkbox with partial-selection feedback.
- Game lookups drop trailing platform words: Windows, PC, Win64, Win32, x64, x86, Linux and Portable.
- Artwork revision 9 refreshes automatic lookups. Images are type-sniffed when a CDN omits Content-Type, including PlayStation art; PlayStation name searches retry shorter queries to handle registered trademark symbols in store names.
- Rail resizing captures the pointer across WebView boundaries, with a wider hover target and touch support. Labels and counts reappear when expanded; saved widths, keyboard controls and double-click toggling remain available.
- Navigation targets have solid hover/active backing, centered icons, compact corner badges, even spacing and a hidden scrollbar.
- Wide artwork keeps its aspect in details, selected cards and Add, fading into a colour wash at large widths instead of zooming; details height grows up to 420px.
- Ordinary filenames keep software and operating-system versions; game cleanup no longer leaves dangling separator tokens.
- Small Steam artwork tries the 616?353 capsule before the store header and displays without enlargement or cropping over a blurred colour wash. Older Steam banner caches refresh automatically.
- Console packages display their resolved game title while keeping the original filename; existing artwork matches also recover the missing title.
- Downloads without wide artwork use a dark, blurred icon backdrop with a sharp icon tile in selected cards, details, Add and completion notifications. Web artwork tries the matched page's wide social image first and repairs missing cached banners.
- Short or generic download names require an exact Steam title; other matches require multiple significant words. Saved incompatible matches are cleared for a fresh lookup.
- Game and application cards use cleaned display titles while retaining the real filename underneath. Finished files hide Resume in details, the selection toolbar and the item menu; torrent sharing remains available.
- Completion notifications keep game art in a fading top strip, with an overlapping icon, dark text area and accent timer. Retry notifications retain a red connection icon and border even for games; unnamed queue countdowns say "Queue finished" and keep a neutral Cancel button.
- Add links have a leading link icon. Bright detected artwork dims adaptively while keeping the left side dark, and yellow artwork produces a warmer gold accent shared with the main window. Start download includes its download icon.
- Details tables and graphs stack earlier in narrow windows; Options controls fit at minimum width, wide connection tables scroll, and footer limit labels wrap with their pickers. Numeric fields, details tables, paths and notification countdown/sharing values use the bundled monospace font.
- Bright game banners are sampled once to keep window backgrounds dark and softly coloured, with muted artwork accents and dark backing behind row pills and banner chips. Details artwork keeps its colour.
- Completed downloads without a completion date show just Done.
- Inactive download cards describe paused, queued, completed, sharing and error states without idle download speeds or connection counts. Footer speed pickers show the full limit text.
- Dialogs keep keyboard focus inside, offer a close button, protect forms from backdrop clicks and wrap footer actions.
- Removal uses one action with an optional Recycle Bin checkbox; prompts and countdowns name their actions, and update notes use readable formatting.
- Settings groups align controls, simplify wording and share speed presets with Scheduler. Theme changes reach open windows and native title bars.
- Small desktop windows keep extra list actions in More, scroll details in their right column, and stack cramped General, Scheduler and Add layouts; the main minimum is now 640x420.
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
