# Contributing to FluxDM

Issues and pull requests are welcome.

## The one rule: update the docs in every commit

Every commit that changes code must also update the docs, in the same commit:

| File | What to add |
|---|---|
| `PROJECT_STATE.md` | A dated entry: what changed, why, and what was tested (and what wasn't). |
| `CHANGELOG.md` | User-visible changes under the version being worked on. This section becomes the release notes. |
| `README.md` / `GUIDE.md` | Any feature, setting or build step that changed. |

Turn on the hook once after cloning, and it will refuse code commits that skip `PROJECT_STATE.md` and `CHANGELOG.md`:

```sh
git config core.hooksPath .githooks
```

AI assistants (Claude and others) follow the same rule; it is also written in `CLAUDE.md`. Don't bypass the hook with `--no-verify`.

## Keep it public-safe

Don't commit local paths, PC names, personal download lists or screenshots of your real desktop.

## Building and testing

See the README's "Build" section. Build from PowerShell (Git Bash's `link` shadows the MSVC linker). Before a pull request:

```powershell
cd src-tauri; cargo test --release
```

Test the app with its own data folder so it never touches your real list: set `FLUXDM_DATA=<some folder>`, give that copy its own `LocalApiPort` and `RemotePort`, and a download folder that isn't your real Downloads.

## Releasing

1. Bump the version in `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json` (and `Cargo.lock`).
2. Write the version's section in `CHANGELOG.md`: `## X.Y.Z — YYYY-MM-DD`.
3. Push a tag `vX.Y.Z`. The release workflow builds the setup and the portable exe and publishes the release with that section as its notes.
