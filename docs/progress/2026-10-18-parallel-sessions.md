# Parallel sessions (2026-10-18)
Goal: make it easier to run several Claude Code sessions on this repository at once (the owner is setting up a Claude Code project, whose threads are cloud sessions, each on its own branch).

Seen before the change:
- Every merge of two sessions' work conflicted in `docs/progress.md`: each rewrote "Current slice" and added its section at the same place (#10 with #11, and again with #12).
- A new cloud container lacks what this session installed by hand: cargo-deny (so "Verify all" cannot run), the toolchain CI uses (the image had an older stable), and the libraries for a real window under Xvfb (`libxkbcommon-x11-0`, `libegl1`, `libgl1-mesa-dri`, `xdotool`).
- Sessions learn what the others do only when the owner relays it.

Done:
- **Progress per slice.** Each slice writes its own `docs/progress/<YYYY-MM-DD>-<slug>.md`; `docs/progress.md` is the log up to today, not added to, its open items struck when fixed. CLAUDE.md "Progress" says so, and a new "Parallel sessions" section: own branch from the latest main, one slice per branch, read open pull requests and the newest progress files first, merge main in (never rebase or force-push a branch with a pull request), write for other sessions in the progress file or the pull request, not in chat. **CLAUDE.md changed, for review** (no invariant touched).
- **`scripts/cloud-setup.sh`**, run by a SessionStart hook in `.claude/settings.json` (startup and resume) only when `CLAUDE_CODE_REMOTE=true`, so local sessions are untouched: `rustup update stable`; cargo-deny 0.20.2 from its release, checksum pinned (a download, not a minutes-long `cargo install`); the window libraries, installed only when missing. It never fails the session: it prints one line, either what it found or what it could not install. It can also be pasted as the cloud environment's setup script, which is cached between sessions.

Checks:
- Merge simulation in a scratch clone, two branches from this commit: each adding its own progress file merges cleanly; each striking a different open item in `docs/progress.md` merges cleanly; the old way (both rewriting "Current slice" and adding a section at the top) conflicts. Striking two **adjacent** items still conflicts (git sees neighbouring lines as one change); rare and trivial to resolve.
- `scripts/cloud-setup.sh`: `bash -n` 0. With everything present: 0.95 s. With cargo-deny removed: reinstalled in about 1 s, `cargo deny check` 0. With `xdotool` removed: reinstalled, 12.4 s. With a wrong checksum: nothing installed and the line says so, exit 0.
- The hook's command, run by hand: with `CLAUDE_CODE_REMOTE=true` it prints the script's line; without, nothing, exit 0.
- Verify all (no Rust changed): fmt 0; clippy workspace and `-p klefki` 0; `cargo test --workspace` 313 core + 144 app passed; `cargo deny check` 0.

Not verified:
- That Claude Code runs the hook: it takes effect in sessions started after this is on their branch. A new thread should show the line `cloud-setup: ...` at its start.
- `rustup update stable` on an outdated image (a toolchain download) was not timed.

Owner's part (UI only; no tool creates a project): claude.ai/code, Projects, New project, repository `DavySc/klefki`, the Default Cloud Environment. Optionally paste `scripts/cloud-setup.sh` into that environment's setup script so its result is cached.

Next: the rest of M1 in PLAN.md (legacy `mcpServersDisabled` shown read-only with a migrate action; secret-looking values masked with a `${VAR}` suggestion; the casing fix action), then M2.
