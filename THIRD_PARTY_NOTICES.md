# Third-party notices

FluxDM's own code is MIT-licensed (see `LICENSE`). It includes or downloads these:

| Component | Where | License |
|---|---|---|
| [librqbit](https://github.com/ikatson/rqbit) 9.0.1 (`librqbit`, `librqbit-peer-protocol`, `librqbit-tracker-comms`), patched | `src-tauri/vendor/` (changes listed in `src-tauri/vendor/PATCHES.md`) | Apache-2.0, `src-tauri/vendor/LICENSE-APACHE` |
| [Tauri](https://tauri.app) and the other Rust crates | `src-tauri/Cargo.lock` | MIT and/or Apache-2.0 (each crate's own) |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | downloaded from its official releases on first video download, not bundled | Unlicense |
| [FFmpeg](https://ffmpeg.org) | downloaded on first video download, not bundled | LGPL/GPL (depending on the build) |
| [Inter](https://github.com/rsms/inter) 4.001, variable UI font | `ui/fonts/Inter.woff2` | SIL Open Font License 1.1, `ui/fonts/Inter-OFL.txt`; Copyright 2016 The Inter Project Authors |
| [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono), variable, Latin subset, for numbers and paths | `ui/fonts/JetBrainsMono.woff2` | SIL Open Font License 1.1, `ui/fonts/JetBrainsMono-OFL.txt`; Copyright 2020 The JetBrains Mono Project Authors |

FluxDM is not affiliated with any of these projects.
