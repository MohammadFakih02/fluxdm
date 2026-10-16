# FluxDM — notes for Claude

Read `PROJECT_STATE.md` first (layout, build and test notes, history), then `GUIDE.md` when you need more.

## RULE 1 — update the docs before every commit (always, without being asked)

Before **every** commit that touches code (`src-tauri/`, `ui/`, the `.ps1` scripts):

1. Add a dated entry to `PROJECT_STATE.md` (what changed, why, what was and wasn't tested).
2. Add the user-visible changes to `CHANGELOG.md` under the version being worked on.
3. Update `README.md` / `GUIDE.md` if a fact in them changed.
4. Stage those files with the code, then commit.

The `.githooks/pre-commit` hook enforces this (`git config core.hooksPath .githooks` once per clone).
Never skip it with `--no-verify`.

## Other rules

- The repo is public: no local paths, PC names or personal details in any committed file.
- Never add Claude/AI attribution lines to commits or pull requests.
- Build from PowerShell (Git Bash's `link` shadows the MSVC linker).
- Test copies use `FLUXDM_DATA=<dir>` and their own ports, and never save into the real Downloads folder.

Reminder: docs first, then commit (RULE 1).
