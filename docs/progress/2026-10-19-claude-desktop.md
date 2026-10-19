# Claude Desktop servers, read-only (2026-10-19)

## Goal
PLAN.md "Nice to have" 1: list the servers in Claude Desktop's `claude_desktop_config.json`, including the Windows
Store (MSIX) location, without ever writing to that file, and without costing anything at start-up.

## Done
- Core `desktop.rs`: `load(&DesktopRoots) -> Desktop`. Looks in the settings folder (`BaseDirs::config_dir()/Claude`:
  `%APPDATA%`, `~/Library/Application Support`, `$XDG_CONFIG_HOME`) and, on Windows, in every
  `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude\`. A packaged app reads its private copy first and
  falls back to the real folder (Microsoft, "Flexible virtualization"), so the Store copy wins when it exists and the
  other copy is listed as not read (`unused`).
- Read like `.mcp.json`: through `fsx::read_limited` (1 MB limit, no symlinks), duplicate names noticed, at most 500
  servers, every string sanitised. Command lines and URLs come from the same `classify` as the Connections list
  (secret-looking arguments `…`, no URL credentials or query). Env and header values are never read into the result,
  only their names (at most 64 each).
- Engine: `Command::Desktop` → `Event::Desktop`, read only when asked; `EngineConfig::desktop` (tests give temp
  folders or none, so tests never see a real Desktop config).
- App: a "Claude Desktop" page in the left menu (and the command palette), read when opened and let go when left,
  like the Activity log. Marked Read-only, says where to change servers in Claude Desktop, shows each server's
  type, command or URL, setting names, and "Also in Claude Code: user, local in 2 projects" for the same name. The
  path shows on hover with Copy path. The top bar filter filters these servers there; F5 reads the file again. The
  whole page scrolls, so it works at 800x480.

## Checks
- Tests written first and seen failing with a stub `load` (9 of 9 failed): `desktop::tests::*` (not found, file
  order and hidden secrets, Store copy wins, Store install without its copy, empty file, invalid/oversized, symlink
  refused, sanitised names and duplicates, caps). Then `engine::claude_desktop_is_read_only_when_asked_and_never_written`
  (no read at start-up; read twice; file bytes, mtime and folder unchanged) and 8 app tests in `desktop.rs`.
- Full verify exit 0 on 99f3ab3 + this branch: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --workspace` (core 673, app 323, cli 18 + 25), `cargo deny check` ok.
- Merged main 4b8fb5a (Secrets page) in: nav, go(), events and the top bar filter keep both pages. Full verify exit 0 again (core 689, app 332, cli 18 + 25), `cargo deny` ok.
- Real app under Xvfb with a temp `HOME`/`CLAUDE_CONFIG_DIR` and a sample config: page at 1200x720 and 800x480.

## Not verified here
- Windows with the real Store install (path and precedence come from Microsoft's docs and decision 003's look at
  the owner's machine); macOS location.
- The log tail PLAN.md mentions for Desktop servers belongs to "Bounded log tail" (item 2), not this slice.

## Next
- Cross-review; then, if wanted, "Add to Claude Code" from a Desktop server through the existing paste/add flow.
