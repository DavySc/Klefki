# Progress

## Current slice
Read-only inventory of the real Claude Code config, restyle (Catppuccin Mocha, JetBrains Mono, bigger controls), and a light theme (Latte-based) with a System option.

## Done (nothing committed yet)
- Workspace `klefki` (renamed from klefki): `crates/core` (`klefki-core`), `crates/app` (`klefki`), `crates/cli` (`klefki-cli`, stub).
- Core:
  - `ServerName` validation, `sanitize` (ANSI CSI/OSC, C0/C1, bidi).
  - `config/fsx.rs`: size-limited reads, refuses symlinks and `.credentials.json`.
  - `config/inventory.rs`: user, local and project (`.mcp.json`) servers from `.claude.json`; honours `CLAUDE_CONFIG_DIR`; status hints from `mcp-health-cache.json` and `mcp-needs-auth-cache.json`; shadowed and casing-collision marks; duplicate-key detection; project path spellings merged.
  - Secrets never reach rows: URL credentials and query strings are stripped, env and header values are never read into rows.
  - UNC and relative project paths are never opened.
- App: loads the real config by default (`--demo` for fixtures); warnings banner; Catppuccin Mocha palette with Klefki accents; JetBrains Mono (embedded, OFL); larger nav, toolbar and rows.
- Light theme: `LIGHT` palette from Catppuccin Latte with deeper accents for links; top-bar dropdown offers System, Light, Dark; choice saved through eframe storage; `--theme` flag overrides for one run. Contrast of every ink on every surface is tested for both palettes.
- `.github/workflows/ci.yml` (fmt, clippy, test on win/mac/linux). Not run yet (never pushed).

## Checks run
| Command | Result |
|---|---|
| `cargo fmt --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test --workspace` | 63 passed (33 core, 30 app incl. egui_kittest), 0 failed |
| grep for `std::fs`, `process::Command`, `std::net`, `open::` in `crates/app/src` | 0 matches |
| `target/debug/klefki.exe` against the real `~/.claude.json`, screenshot | lists `REMOTE_1`, `REMOTE_2` (Connected, from Claude's health cache) and `azure-devops` |
| `target/debug/klefki.exe --demo`, screenshot | all scopes, badges, group headers render in full |
| `klefki.exe --demo --theme light`, dropdown opened, screenshot | dropdown shows current choice, lists System, Light, Dark with icons, current one highlighted |
| `klefki.exe --demo --theme light` and `--theme dark`, clicking a row, screenshots | both themes render nav, toolbar, grid, badges, selection and detail panel legibly |

## Findings fixed along the way
- Hover moved things. Root cause (egui 0.36, `widget_style.rs`/`button.rs`): an unframed item (selectable tab, list option) is laid out with its inactive stroke width subtracted from the padding but not painted, then the frame and stroke appear on hover, so it grew 1 px per side. The earlier `expansion` and stroke-width changes did not touch this (egui 0.36's defaults already have zero expansion). Fix: `flat_items()` removes strokes for tabs and the theme dropdown options. Reproduced first by tests that measure the item rectangle before and while hovered.
- Dark: muted text on the hovered-control fill was 4.10:1; hover fill darkened to `#3B3D50`.
- Light: Latte's own green, peach, pink and gold fail 4.5:1 as text, so the light accents are deeper variants; the warn colour and the selection fill were tuned after the contrast test caught them.
- kittest forces its own theme after the app is built; the light-theme tests pass the theme into the harness. Palettes are compared by colour, not pointer (consts have no stable address).
- Original palette danger colour failed contrast; the Mocha palette passes the 4.5:1 test on every surface.
- Nav items had no accessibility info; added.
- Selectable labels swallowed row clicks; disabled globally.
- The test harness rendered a frame before the theme (fonts) was applied; tests now build through `build_eframe` like the real app.
- The thin bar on the dropdown popup in light mode was the same bug (an option growing past its scroll area); it disappeared with the fix.
- The grid only renders rows in view; the test window was enlarged for the taller rows.
- A UI test wrote a temp config (IO in `crates/app`); replaced by an in-memory snapshot. Real-config loading is tested in core.

## Not done / unverified
- Theme persistence goes through eframe's storage (written by the framework to the app data folder); the app crate itself still has no fs calls. Not tested across a real restart, and flagged for review against the no-IO invariant.
- System theme follows the OS setting; only checked that it is selectable, not against a real OS theme change.
- Hover colours were only seen on one row in each theme.
- User-scope servers are always shown as enabled. Where Claude Code stores "disabled" for user servers is the M1 day-1 spike (needs a live `/mcp` toggle).
- Status is only Claude's cached hint, not a live `claude mcp get` check.
- The CLI version in the top bar is not detected yet (hidden unless known).
- CI untested on macOS/Linux. Claude Code lock behaviour not investigated yet.
- `ConnectionId` (v1-compatible) not implemented: needs the v1 source to confirm the encoding.
- Protected files not created, proposals in `docs/proposals/`: `crates/app/clippy.toml`, `deny.toml`, `crates/fake-claude/`, `fixtures/malicious/`. `cargo deny` not run.
- Acceptance tests were written together with the code, not seen failing first.
- Only 3 user-scope servers exist in the real config; local and project listing is tested on temp configs only.

## Next
1. Review the look; tune `theme.rs`.
2. Approve the protected-file proposals.
3. M1 day-1 spikes (lock experiment, disabled-servers location), recorded in `docs/decisions/`, then the CLI locator/runner and live status.
