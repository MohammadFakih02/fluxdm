<div align="center">

<img src="ui/icon.png" width="88" alt="FluxDM icon">

# FluxDM

[![Release](https://img.shields.io/github/v/release/MohammadFakih02/fluxdm?label=download&color=4f8cff)](https://github.com/MohammadFakih02/fluxdm/releases/latest)
[![CI](https://github.com/MohammadFakih02/fluxdm/actions/workflows/ci.yml/badge.svg)](https://github.com/MohammadFakih02/fluxdm/actions/workflows/ci.yml)
![Windows 10/11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078d4)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)

</div>

A fast download manager for Windows: multi-connection downloads that survive pauses and reboots, videos from
YouTube and 1000+ sites, and a built-in BitTorrent client, all in one small exe with nothing else to install.

FluxDM 2 is a rewrite of the original .NET/WPF FluxDM in Rust + Tauri. It reads the same data folder
(`%APPDATA%\FluxDM`), so your download list, queues, settings and statistics carry straight over.

For the full picture (how it's built, the browser extension and its signing, releasing), see `GUIDE.md`.

## Install

Download the setup or the portable exe from the [latest release](https://github.com/MohammadFakih02/fluxdm/releases/latest).
The files aren't code-signed, so Windows SmartScreen may say "Windows protected your PC": click **More info → Run anyway**.

- **Setup:** run `FluxDM-Setup-<version>.exe`. It installs for your Windows account only (no admin) and adds
  a Start Menu entry and the `.torrent` file association.
- **Portable:** put `FluxDM.exe` anywhere and run it. Create an empty `portable.txt` next to it to keep the
  data in a `data` folder beside the exe instead of `%APPDATA%\FluxDM`.

It runs on Windows 10/11. The only runtime it uses is Microsoft Edge WebView2, which comes with Windows 11 and
current Windows 10. If it's missing, the setup installs it.

## What it does

- **Files:** up to 64 connections per download. Each connection writes straight into the file, and the
  progress of every connection is saved, so a pause, crash or reboot resumes where it stopped.
  - When a connection finishes early, it steals half of the slowest one's remaining work.
  - Backup links (mirrors) are used in parallel. Each one is checked to serve the same bytes before it helps.
  - MD5 / SHA-1 / SHA-256 / SHA-512 verification; Hugging Face's published SHA-256 is picked up automatically.
  - Fresh links for expired downloads keep the bytes you already have.
- **Videos:** yt-dlp + ffmpeg, fetched once from their official releases on first use. Pick a quality up to
  4K, audio-only MP3/M4A, or whole playlists. Browser cookies can be used for age-restricted or members-only
  videos.
- **Torrents:** magnet links, .torrent links and files, using librqbit.
  - TCP + µTP, DHT, UPnP and fast resume.
  - Pick which files to download.
  - Separate upload limits while downloading and while seeding, and a seed-ratio target.
  - Force recheck/reannounce, move data, add trackers, export .torrent, copy the magnet link.
  - **Download in order** (right-click): fetches each file from its first piece to its last, so a video can be watched while it downloads.
  - Set location on an unfinished torrent moving to another drive offers to start over there instead of copying the full-size placeholder files.
- **Organising:** categories, tags, queues with a scheduler that can start and stop queues at set times on
  chosen days. When a queue finishes, FluxDM can exit, sleep, hibernate, shut down or restart, with a
  60-second countdown you can cancel.
- **Speed:** a global limit, per-queue and per-download limits, and a daily speed schedule.
- **Catching downloads:**
  - Copying a download link opens the Add window.
  - Drag links, `.txt` lists or `.torrent` files onto the window.
  - The browser extension and bookmarklets send downloads to FluxDM. The Firefox build is signed by Mozilla,
    so Settings → Browser extension installs it in one click. Chrome, Edge, Brave, Opera and Vivaldi load it
    unpacked (the button opens everything needed).
  - FluxDM can become the system's `magnet:` handler. If Windows still has another app (e.g. qBittorrent) set as the default, FluxDM opens Settings → Default apps for you to pick FluxDM for MAGNET.
- **Blank-window protection:** the app keeps WebView2 rendering, repaints a freshly shown window, and has a tray item *Reload window* if it ever looks blank.
- **Phone remote:** a PIN-protected web page on your home Wi-Fi with a QR code to pair. No app needed.
- **Updates:** Settings → Updates checks this repository's releases (any `owner/repo` can be set).

What's new in each version: [CHANGELOG.md](CHANGELOG.md). FluxDM downloads the new exe,
  verifies its SHA-256 and restarts into it.

## Build

You need Rust (MSVC toolchain), Visual Studio Build Tools (MSVC + Windows SDK) and, for the setup, the Tauri CLI
(`cargo install tauri-cli`). Run these from PowerShell; Git Bash's `link` shadows the MSVC linker.

```powershell
.\package-release.ps1 # dist\share\: setup + install notes, and the source zip, to hand out
.\pack-extension.ps1 # dist\FluxDM-extension-firefox.zip / -chromium.zip for the add-on stores
.\install.ps1        # build and install for this user (%LOCALAPPDATA%\FluxDM)
.\build-setup.ps1    # dist\FluxDM-Setup-<ver>.exe, dist\FluxDM.exe, dist\FluxDM.exe.sha256
cd src-tauri; cargo test
```

## Publishing an update

1. Bump the version in `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
2. Push a tag `v<version>`. The GitHub Actions workflow builds the setup and the portable exe and attaches
   them, with the `.sha256`, to a release.
3. Copies of FluxDM pick it up through *Check for updates* (the update source defaults to `MohammadFakih02/fluxdm`).

## Command line

| | |
|---|---|
| `FluxDM.exe <url or magnet or .torrent path>` | opens the Add window (or hands it to the running copy) |
| `FluxDM.exe --tray` | starts hidden in the tray (used by *Start with Windows*) |
| `FluxDM.exe --exit` | asks the running copy to pause everything, save and quit |
| `FLUXDM_DATA=<folder>` | uses another data folder; such a copy runs beside the normal one (for testing) |

## Credits

[librqbit](https://github.com/ikatson/rqbit) (Apache-2.0), [yt-dlp](https://github.com/yt-dlp/yt-dlp)
(Unlicense, downloaded at runtime), [FFmpeg](https://ffmpeg.org) builds by gyan.dev (GPL, downloaded at
runtime), [Tauri](https://tauri.app) (MIT/Apache-2.0).

## Contributing

Every commit that changes code must also update `PROJECT_STATE.md` and `CHANGELOG.md`. Run
`git config core.hooksPath .githooks` once after cloning to turn on the check.

## License

[MIT](LICENSE) © 2026 Mohammad Fakih. The patched librqbit crates in `src-tauri/vendor/` are Apache-2.0
([LICENSE-APACHE](src-tauri/vendor/LICENSE-APACHE)); see `src-tauri/vendor/PATCHES.md` for what was changed.

