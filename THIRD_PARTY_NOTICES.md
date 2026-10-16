# Third-party notices

FluxDM's own code is MIT-licensed (see `LICENSE`). It includes or downloads these:

| Component | Where | License |
|---|---|---|
| [librqbit](https://github.com/ikatson/rqbit) 9.0.1 (`librqbit`, `librqbit-peer-protocol`, `librqbit-tracker-comms`), patched | `src-tauri/vendor/` (changes listed in `src-tauri/vendor/PATCHES.md`) | Apache-2.0, `src-tauri/vendor/LICENSE-APACHE` |
| [Tauri](https://tauri.app) and the other Rust crates | `src-tauri/Cargo.lock` | MIT and/or Apache-2.0 (each crate's own) |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | downloaded from its official releases on first video download, not bundled | Unlicense |
| [FFmpeg](https://ffmpeg.org) | downloaded on first video download, not bundled | LGPL/GPL (depending on the build) |

FluxDM is not affiliated with any of these projects.
