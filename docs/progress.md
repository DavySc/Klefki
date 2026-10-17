# Progress

## Current slice
Live status checks (CLI locate/runner/parse, status engine, UI wiring). Done, see "Live status" below. Next: fix the inventory's enabled/disabled semantics (decision 002), then `docstore`.

Earlier slice: M1 day-1 spikes done and recorded (below). Next: fix the inventory's enabled/disabled semantics, then `docstore`.

Previous slice: Read-only inventory of the real Claude Code config, restyle (Catppuccin Mocha, JetBrains Mono, bigger controls), and a light theme (Latte-based) with a System option.

## Live status (2026-10-17)
- Cause of the original "Unknown" on `azure-devops`: Claude's health cache only holds remote servers, so a stdio server has no hint. Claude itself reports it Connected (`claude mcp get`, about 7 s).
- Core: `cli/locate.rs` (native executable only, never `.cmd`/`.bat`/`.ps1`, relative PATH entries ignored), `cli/runner.rs` (no shell, `CREATE_NO_WINDOW`, `NO_COLOR`, timeout and kill, capped and sanitised output, secret only in env), `cli/parse.rs` (reads `mcp get`; keeps only the status and a clipped issue line), `engine.rs` (engine thread, 3 workers, events).
- Behaviour: at startup only servers whose status Claude's cache does not know are checked (unknown first); Refresh (button or F5) re-checks everything; project (`.mcp.json`) servers only through **Check now** in the Status tab, with a warning that it runs the repository's command. Names that are not safe never reach a spawn.
- Statuses added: Degraded, PendingApproval, NotConfigured, Checking; "Unknown" now reads "Not checked".
- Fixtures: `fixtures/mcp-get/` (real captures from 2.1.302 plus clearly marked synthetic and hostile ones).
- Approved protected files created: `crates/fake-claude` (test stub, `docs/proposals/fake-claude.md`) and `crates/app/clippy.toml` (UI does no IO; proven by a deliberate violation failing clippy).
- Security hardening beyond the plan: a server name may not start with `-` (it would be read as a CLI option), and the plan's regex is otherwise unchanged.

## Debugging connections (2026-10-17)
Decision 004 compares the MCP debugging guide with this plan. The **Diagnose** action is built and was run in the real window against deliberately broken servers:
- Core: `diagnose/redact.rs` (secrets removed; keeps the "set / not set" summary of proxy variables), `diagnose/hints.rs` (16 plain-language hints including the 2026-07-28 protocol errors; the cases that fooled a naive match are tests: "No token data found" in a healthy log, "timeout of 30000ms" as a normal line, "command not found" as a bad URL), `diagnose/report.rs` (the lines about one server, what the server itself printed, clipped, most recent last), and `Command::Diagnose` / `Event::Diagnosis` in the engine. It runs `claude --debug mcp --debug-file <private temp file> mcp get <name>`, reads the file with a size cap, deletes the folder, and also updates the server's status.
- App: Status tab has **Check now** and **Diagnose** side by side, "What probably went wrong", "The server said", a collapsed "Client log", and **Copy report** (plain text).
- Real window: a stdio server whose command does not exist shows "The command was not found" plus the real `is not recognized as an internal or external command` message; an HTTP server on a closed port shows "Nothing is listening at that address". No private temp folders or `mcp get` processes were left behind.
- Protected `crates/fake-claude` got the two approved additions (`"*"` wildcard argument, `debug_file_from`).
- Layout bug found and fixed in the real window: a long status detail or target widened the whole detail panel (content jumped from x=584 to x=233) because grid cells do not wrap. Reproduced by two tests first; the grids now cap their column width and the values wrap.
- Tests that depended on a tight process-start time were flaky under load (800 ms timeouts; a 400 ms overlap check); they now use roomy timings with the same assertions, and 10 consecutive full runs passed.

## Enabled/disabled and approval (2026-10-17)
Fixed the inventory per decision 002: the toggle is `disabledMcpServers` for every scope; a user server shows how many known projects have it off ("off in 3/17", disabled only when off everywhere); project servers get a separate approval (approved, pending, rejected) read from both `.claude.json` and the repository's `.claude/settings.local.json`. One earlier test encoded the old, wrong rule and was replaced for that reason.

## Brainstorm comparison (2026-10-17)
An outside brainstorm was compared with the plan and checked on this machine: `docs/decisions/003-brainstorm-comparison.md`. Four ideas were added to PLAN.md as nice-to-haves after M4 (read-only Desktop detection, bounded redacted log tail, paste-to-add, VS Code visibility hint); `cmd /c` validation and log cleanup were not adopted. Measured release build: 0.7 to 0.8 s to a window, 109 to 117 MB working set.

## Spikes (2026-10-17, Claude Code 2.1.302, Windows)
- **Locking** (`docs/decisions/001-claude-json-locking.md`): Claude takes a `<claude.json>.lock` directory (proper-lockfile, stale 10 s), re-reads under the lock and preserves unknown keys and order (observed). It waits only about 1.5 s for someone else's lock, then writes **without** the lock, so klefki must hold the lock briefly, abort rather than write unlocked, and re-read about 1 s after writing.
- **Disabled servers** (`docs/decisions/002-disabled-servers.md`): the real `/mcp disable` writes `projects[<key>].disabledMcpServers` in `~/.claude.json`, per project, for user, local **and** `.mcp.json` servers. Approval of `.mcp.json` servers is separate (`enabledMcpjsonServers`, now in `<project>/.claude/settings.local.json`). This contradicts PLAN.md and a CLAUDE.md invariant: proposals in `docs/proposals/spike-findings.md`, awaiting review.
- Evidence: reproducible scripts and a PTY driver in `docs/decisions/spikes/`; both reproducer scripts were re-run from the repo and gave the same results. All experiments used a throwaway `CLAUDE_CONFIG_DIR`.
- Found by the spikes: the inventory code is wrong for enabled/disabled (project rows use `disabledMcpjsonServers`; user rows always enabled; approval state not shown). Not fixed yet.

## Done before the spikes (nothing committed since e422f56 except what is listed here)
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
| `cargo test --workspace` | 176 passed (125 core, 51 app incl. egui_kittest), 0 failed |
| grep for `std::fs`, `process::Command`, `std::net`, `open::` in `crates/app/src` | 0 matches |
| `target/debug/klefki.exe` against the real `~/.claude.json`, screenshot | lists `REMOTE_1`, `REMOTE_2` (Connected, from Claude's health cache) and `azure-devops` |
| `target/debug/klefki.exe --demo`, screenshot | all scopes, badges, group headers render in full |
| `klefki.exe --demo --theme light`, dropdown opened, screenshot | dropdown shows current choice, lists System, Light, Dark with icons, current one highlighted |
| `klefki.exe --demo --theme light` and `--theme dark`, clicking a row, screenshots | both themes render nav, toolbar, grid, badges, selection and detail panel legibly |

## Findings fixed along the way
- The plan's name regex allows a leading `-`; passed as `claude mcp get <name>` it would be read as an option. Rejected in `ServerName` (test first).
- The project-server check runs the repository's command, so it is never automatic.
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
- Diagnose output formats are from Claude Code 2.1.302 only; the hints match short stable fragments and say nothing rather than guess when the text changes.
- The debug file did not repeat a configured header or env secret in two tests, but that is not a guarantee; redaction is the safeguard, with tests.
- The runner kills only the `claude` process on a timeout, not any grandchildren it started (for example `npx` and node for a stdio server). A Windows job object would fix it; not done. Timeouts are rare (30 s) but could leave a server process behind.
- A refresh that arrives while checks run drops their answers by generation; this is covered by reading the code and by the UI test for cut-off checks, not by an engine test with a real race.
- Live checks use the user's home folder as the working directory for user-scope servers; a local server named like a project server in the home folder could shadow it. Not seen in practice.
- `cargo deny` still not run (`deny.toml` and `cargo-deny` pending).
- Theme persistence goes through eframe's storage (written by the framework to the app data folder); the app crate itself still has no fs calls. Not tested across a real restart, and flagged for review against the no-IO invariant.
- System theme follows the OS setting; only checked that it is selectable, not against a real OS theme change.
- Hover colours were only seen on one row in each theme.
- Enabled/disabled in the grid is wrong for user and project rows (see Spikes); fix is the next slice.
- Status is only Claude's cached hint, not a live `claude mcp get` check.
- The CLI version in the top bar is not detected yet (hidden unless known).
- CI untested on macOS/Linux. Spike findings are for Claude Code 2.1.302 on Windows only; a live interactive session was not tested for outside-edit survival.
- `ConnectionId` (v1-compatible) not implemented: needs the v1 source to confirm the encoding.
- Protected files not created, proposals in `docs/proposals/`: `crates/app/clippy.toml`, `deny.toml`, `crates/fake-claude/`, `fixtures/malicious/`. `cargo deny` not run.
- Acceptance tests were written together with the code, not seen failing first.
- Only 3 user-scope servers exist in the real config; local and project listing is tested on temp configs only.

## Next
1. Review the look; tune `theme.rs`.
2. Approve the protected-file proposals.
3. M1 day-1 spikes (lock experiment, disabled-servers location), recorded in `docs/decisions/`, then the CLI locator/runner and live status.
