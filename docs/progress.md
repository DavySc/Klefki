# Progress

## Current slice
UX pass (2026-10-18). Done; see below. Next: the edit form, duplicate, keyring secrets.

## UX pass (2026-10-18)
First look at the real window: this container can run it now (`apt-get install libxkbcommon-x11-0 libegl1 libgl1-mesa-dri xdotool`, then `xvfb-run`), driven with `xdotool` against a throwaway `CLAUDE_CONFIG_DIR`. Screenshots before and after are in `docs/screenshots/`. The Linear article could not be fetched from here (network policy); the changes follow its spirit (calmer chrome, content first), Raycast (keyboard first, a command palette) and Excalidraw (friendly empty states).
- Seen in the real window and fixed: 7 of 11 toolbar buttons did nothing ("not built yet"), and "0 selected" wrapped to its own line; the detail panel took half the window (five tabs did not fit) and squeezed out Status and Target; long project paths in group headers ran across the grid and into the detail panel; "Pending approval" was cut off; the review led with a raw JSON path, said "the 1 projects", shouted the restart hint in orange, and its button looked like any other; nothing was reachable by keyboard except F5, Del and Ctrl+E; unbuilt pages and tabs said "Not built yet".
- Command palette (Ctrl+K, ⌘K on macOS; `app/src/command.rs`): actions on the selection first (turn on/off, edit, check, diagnose, approve, delete), then refresh, filter, theme, and "Go to" every server. Letters-in-order matching that favours word starts and runs; arrows, Enter, Esc; mouse too. Each row is one accessible entry.
- Arrow keys walk the list; Ctrl+F focuses the filter; neither acts while a text field has the keyboard.
- Review dialog: plain sentences first ("Turn off github in 3 projects", from core: `PlanView.summary`), the confirm button names the action ("Turn off", "Delete" in red, "Approve", "Save"; "Apply" for a mix), Enter confirms, the restart hint is a quiet note (`PlanView.notes`), the exact lines follow under "Changes in .claude.json". "Saving…" while waiting (text, not a spinner, so nothing repaints every frame).
- The result is a toast in the bottom-right corner instead of a banner that pushed the list down; a success fades after 4 seconds, a problem stays until dismissed.
- Toolbar shows only actions that work; the selection count only with a selection; the palette button lives in the top bar. Group headers: folder name, count, then the rest of the path muted and cut to fit (whole path on hover and as the accessible label). Tabs: General, Status, JSON, Env, Auth (fit on one line); the detail panel is 280-460 px. Unbuilt pages are marked "soon" in the menu and say what they will do, with a way back; the Env and Auth tabs point to the JSON tab. With nothing selected the detail panel shows the keys.
- **Tests changed, for review:** three tests named toolbar buttons or tab labels that no longer exist: `nav_and_toolbar_targets_are_large` measured the Add button (now Edit), `every_toolbar_button_stays_reachable_next_to_the_detail_panel` listed Add and Import (now the four buttons shown), `detail_tabs_do_not_change_size_on_hover` hovers "Env" (was "Headers/Env"). Each assertion is unchanged; comments in the tests say what changed.
- New tests (15): palette runs a command, goes to a server and acts on it, arrows and Esc, list walking, no walking while typing, plan summary and button, Enter, success fades and errors stay, "soon" pages lead back, key hints, toolbar shows only what works; core: summary, verb, destructive, notes, "1 project". Breaking Enter-to-confirm, arrow walking or the fade each fails a test.
- Real window, seen: the palette with a selection, the review ("Turn off filesystem in 1 project", Turn off button), the saved result and the file written, the light theme. Xvfb without a window manager sometimes does not give the window the keyboard; one run went through palette → Enter → Enter end to end, others lost keys after a click.
- Checks: fmt 0; clippy workspace and `-p klefki` 0; `cargo test --workspace` 250 core + 92 app passed; `cargo deny check` 0.

## File watcher, delete and edit (2026-10-18)
- `watch.rs`: polls the stamps (size, modification time, and the inode on Unix) of `.claude.json`, the two Claude caches, and each known project's `.mcp.json` and `.claude/settings.local.json`, once a second; reports a change once the files have held still for 300 ms, so a burst of saves is one reload. Polling instead of OS notifications: a few dozen `stat` calls a second, the same on every platform and on network drives, no new dependency. UNC and relative project paths are not watched (like the inventory). The baseline is taken right before every reload, so Klefki's own saves are not read twice and a change made during a read is noticed afterwards.
- Engine: the watcher and the UI feed one input channel (a small relay thread passes the UI's commands on and says when the UI is gone). A change on disk reloads like a save does: new snapshot, checks only for servers that became enabled. The verify thread no longer reads the inventory itself; it asks the engine thread to reload.
- Delete: `Change::Delete` for user and local servers (every spelling of the project). `docstore` removes the members in place, commas included, for any set of members (checked against the parsed result for every subset of up to six members, pretty and on one line). Project servers are refused: they live in the repository's `.mcp.json`. OAuth servers are refused (they are changed through Claude's own command, M2). The plan warns that a sign-in Claude keeps is not removed. Toolbar button and the Del key (not while typing).
- Edit: Raw JSON tab. `Command::Inspect` sends the definition with every env and header value shown as `<hidden>`; left as `<hidden>` they keep their value, a new value replaces it, `<hidden>` for a name that had no value is refused. `Change::Replace` is validated (types, url schemes per type, no line breaks in commands, arguments, env or header values, valid env/header names, no `oauth`), size-limited (64 KiB), and the plan shows a line diff of the hidden view (a changed hidden value is reported as such, never shown). Project servers are shown read-only. Toolbar Edit button and Ctrl+E open the tab; Review and save / Revert.
- Bug found by a test and fixed: with a row selected, the detail panel took the toolbar's right half and the buttons past its edge (Delete and later) were cut off and unclickable. The toolbar now wraps; a test checks every button stays left of the detail panel.
- Not done: the structured edit form (Raw JSON only), rename (M2 move), deleting a project server (by design), a sign-out on delete.
- Server names are written as the inventory shows them (sanitised, clipped to 512 characters); a name with control characters or longer than that cannot be deleted or edited from Klefki (the plan reports it as not found).
- Checks: fmt 0; clippy workspace and `-p klefki` 0; `cargo test --workspace` 246 core + 78 app passed, twice; `cargo deny check` 0. Tests were written before the code and failed first where there was a behaviour to miss (watcher off, no rebase, tail-removal rule, toolbar overflow).

## Performance pass 2 (2026-10-17)
Measured with a throwaway release benchmark (median of 5-9 runs, counting allocator; not committed, saved outside the repo). Synthetic configs, Linux, local disk.

| | 61 MB before | 61 MB after | 7 MB before | 7 MB after |
|---|---|---|---|---|
| `docstore` prepare | 144 ms | 86-94 ms | 11.8 ms | 7.0 ms |
| verify, lock held | 87 ms (whole check) | 0 ms (check reads without the lock) | 7.1 ms | 0 ms |
| apply, lock held (all projects) | 384 ms | 182-352 ms (write-back noise; see below) | 21.5 ms | 18.3 ms |
| peak heap reading the file | 67.1 MB | 60.8 MB (= file size) | 8.4 MB | 8.4 MB |
| sanitize 1 MB | 6.3 ms | 0.10 ms | | |
| hints over 1 MB | 5.8 ms | 3.5 ms | | |
| Diagnose report, 1 MB log | 21.9 ms | 16.3 ms | | |

Apply under the lock at 61 MB now splits into read 46 ms, edit 22 ms, write 18 ms (up to 210 ms when the kernel throttles repeated 60 MB writes), fsync 55-61 ms, link + rename + rotation under 0.3 ms. What is left is the file system.

UI (kittest, release, one `step` with no painting): frame time 0.28 / 0.46 / 2.95 ms for 30 / 300 / 3000 rows before, 0.26 / 0.26 / 0.45 ms after; allocations per frame 1500 / 6896 / 82515 before, 1116 / 1236 / 1236 after (the rest is egui's own). A status event: 52 / 502 / 5002 allocations before, none of klefki's own after.

- Engine: the CLI version is asked once per CLI build (path, size, modification time), not on every refresh: each ask was a Node process start. After klefki saves a change it reloads the snapshot only; it no longer looks for the CLI again, asks its version, drops running checks and re-checks every unknown server. Servers the change switched on are checked. Three tests, which failed on the old behaviour (counted from fake-claude's log).
- `docstore`: one pass over the file for root, projects and the targeted entries (`jsonspan::object_with`); on files of 1 MiB or more the JSON check runs on a second thread alongside the scan, and the hash in `prepare` alongside the edit. `verify` reads without the lock and only takes it to write (two tests: no wait while Claude holds the lock; still no write without it). The engine reads `.claude.json` once for both the inventory and the plan.
- `fsx`: the read buffer is sized from the metadata, so peak heap equals the file size.
- `sanitize`: plain runs (printable ASCII, tab, newline) are copied whole, tested 16 bytes at a time in a form the compiler vectorises; checked against the previous implementation on 20,000 generated strings.
- `hints`: one `RegexSet` pass instead of 16 regex passes.
- `inventory`: shadowed check without an O(n²) scan with an allocation per comparison; names lower-cased once for the casing check.
- UI: grid rows are cached per snapshot and search; live statuses are stored by row position, so drawing a row needs no key; status and diagnosis events compare ids in place (`ServerRow::is`); the detail panel no longer clones the selected row each frame. Two new tests (results and selection follow reordered rows; the grid follows a new snapshot and search); removing the cache invalidation fails 25 tests.
- Considered and not done: streaming the inventory parse (peak heap 62 MB to 0.8 MB, but 2.5 times slower: 161 vs 63 ms); memory-mapping the file (unsafe, and a truncating writer would crash the app); a faster non-cryptographic hash (SHA-256 already runs at about 1.3 GB/s and is off the critical path); prefiltering the redaction rules (a later rule could match text an earlier one produced; not worth the risk on a security path); dropping egui's default fonts (they are the fallback for emoji and symbols).
- Checks: fmt 0; clippy workspace 0 and `-p klefki` 0; `cargo test --workspace` 212 core + 70 app passed, three runs in a row; `cargo deny check` 0.

## Lock time and lighter reads (2026-10-17)
Measured on synthetic configs (release build, Linux, local disk; throwaway benchmark, not committed). "Lock held" is `apply` only.

| | 7 MB before | 7 MB after | 61 MB before | 61 MB after |
|---|---|---|---|---|
| `docstore` apply, lock held | ~95 ms (est.: prepare+apply minus prepare) | 16-24 ms | ~790-820 ms (est.) | 340-420 ms |
| `docstore` prepare | 21 ms | 13 ms | 255-307 ms | 160 ms |
| `inventory::load` | 16 ms | 6 ms | 167 ms | 71 ms |

- `.claude.json` is now edited in place: `config/jsonspan.rs` (new) finds the byte ranges of the project entries and their lists, and only the changed lists are rewritten or inserted, indented like their surroundings (or on one line where the file is on one line), with the file's own newline. Everything else stays byte for byte, including formatting serde would have changed (`1.0`, `\u00e9`, numbers beyond u64, four-space indents). One pass over the projects however many ops ("switch everywhere"). The whole text is still checked as valid JSON first. New: a project key or a list that appears twice is refused instead of silently dropping one copy.
- String scanning uses `memchr` (vector instructions where the CPU has them): the projects scan went from 118 ms to 28-34 ms at 61 MB. New direct dependency `memchr` (already in the tree through `regex`; `cargo deny` ok).
- Backups are a hard link to the old file (no copy, any size), made private after the rename; a copy of the text read is the fallback when linking fails. The copy fallback is not exercised by a test (needs a second file system).
- The new file is written from the original text and the replacements, piece by piece, so no second full copy is built in memory; no more full-text clones for the hash or the backup.
- `inventory`: `.claude.json` is read through a pruning deserializer that keeps only `mcpServers` and, per project, the five keys the inventory uses; history and caches are validated but skipped.
- Tests first: byte preservation, one-line entries, duplicate project / list refusal and the hard-link backup failed before the change. A differential test (400 generated files × op sequences, pretty and compact) checks the in-place edit against the previous Value-based edit (kept in the tests as the oracle; its one difference, creating an empty list when enabling, is noted there). Deliberate breaks (indent, rejection clearing, pretty rendering) each made tests fail.
- **Test fixture changed, for review:** `style_is_kept_or_defaults_to_claudes` used a file of CRLF lines ending in a bare LF and asserted every newline came out CRLF; the old full rewrite normalised it. In-place edits keep that bare LF, so the fixture now ends in CRLF (same assertions), and a new test pins the mixed case (kept as is).
- Checks: fmt 0; clippy workspace 0; `cargo test --workspace` 204 core + 68 app passed; `cargo deny check` 0.

## Approval writes (2026-10-17)
- `Change::Approve { id }` for a project (`.mcp.json`) server. The plan lists everything the server would run, in full: command, every argument, environment names **and values**, or the address and any `headersHelper` command (header values are not shown). It refuses, with a reason, what cannot be shown whole and exactly: control or bidi characters (a newline too), a value over 2000 characters, an address with a login, an unknown `type`, wrong-typed fields.
- Writes `enabledMcpjsonServers` (and removes the name from `disabledMcpjsonServers`) in `<project>/.claude/settings.local.json`, creating the file and the `.claude` folder when missing, through `docstore` (`Kind::ProjectSettings`: same lock, backup under its own name, atomic replace; a linked `.claude` folder or file is refused). If the server was rejected in the project entry of `~/.claude.json`, that rejection is cleared too, shown as a second file in the diff. Nothing else in the repository is touched (tested: only `.claude/settings.local.json`, no lock left).
- The definition is checked again at Apply (digest of the `.mcp.json` entry). If the repository edited it after the review, nothing is saved. This is a small window closed, not eliminated: the check and the write are not one atomic step.
- Several files in one plan are written one after the other, settings first and `~/.claude.json` last. If a later one fails, the message names what was already saved.
- Warnings: trust the repository; `.claude/settings.local.json` not named in `.gitignore` / `.git/info/exclude` (only common spellings are recognised, global ignore files are not read, and the warning says so).
- UI: **Review and approve…** in the General tab for pending and rejected project servers; the dialog shows the "would be allowed to run" box before the diff, and the button reads **Approve**.
- Checks: `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0 (and `-p klefki` 0); `cargo test --workspace` 189 core + 68 app passed; `cargo deny check` 0. Broken on purpose, each made tests fail: no re-check of the definition, no clearing of a rejection, no review box, approve button for approved servers.
- Not verified: the exact file Claude Code writes for approval beyond decision 002's one observed file (key names `enabledMcpjsonServers` / `disabledMcpjsonServers` come from there and from Claude's schema); that Claude Code then actually runs the server; Windows paths; the real window; a new file gets a final newline and 2-space JSON, which is a guess for this file. Revoking or rejecting from klefki is not built (the plan only allows writing to approve).

## Docstore and enable/disable (2026-10-17)
- `config/docstore.rs`: Prepare (`prepare`, writes nothing) → plan (only list names, never server settings) → `apply`: take Claude's `<claude.json>.lock` directory (stale after 10 s, waits up to 3 s, aborts rather than write unlocked), re-read, re-apply the ops to the fresh copy, refuse if the file changed in a way that alters what the user saw, back up the old bytes (private, last 20, own folder), write a temp file in the same folder, fsync, rename. `verify` (1 s later) re-applies up to 3 times if Claude overwrote it without the lock. Keeps key order, unknown keys, floats, BOM, CRLF and a final newline if the file had them; default is Claude's own style. Refuses symlinks, wrong shapes, unknown projects (never creates a project entry; that part of Claude's behaviour is unverified).
- `ops.rs` + engine: `Command::Prepare(Vec<Change>)` / `Apply(id)` / `Discard(id)`, events `PlanReady`, `PlanFailed`, `Applied`, `ApplyFailed`, `Reapplied`. A user server is switched in every known project (with the "projects first opened later are not covered" warning); local and project servers in their own project. Project servers: only `disabledMcpServers` in `~/.claude.json`; the repository is never touched, and enabling an unapproved one warns that it is still not approved. Approval writes (`settings.local.json`) are not built.
- UI: Enable and Disable toolbar buttons → Review dialog with the exact lines, warnings, Apply / Cancel (Esc or a click outside cancels), then a banner with the result.
- Reproducers first where a bug existed; for new code the tests passed on first run, so I proved they bite by breaking the code on purpose and watching them fail: no backup, no changed-file check, no lock, first spelling only (docstore); user scope only first project (engine); no double-apply guard, toggle inverted, no discard on cancel (UI). One guard (`plan.is_none()` on the toolbar) cannot be told apart by a test because the modal already blocks the click.
- `deny.toml` (protected file, created at the owner's request; please review) and `cargo-deny 0.20.2` installed. Licences allowed: MIT, Apache-2.0 (+LLVM), BSD-2/3, ISC, Zlib, MPL-2.0, Unicode-3.0, OFL-1.1, Ubuntu-font-1.0 (egui's default fonts), BSL-1.0. Workspace crates have no licence field, so `[licenses.private] ignore = true` is set; the owner should pick a licence instead.
- **Resolved 2026-10-17: owner set `unmaintained = "workspace"` in `deny.toml`; `cargo deny check` now exits 0 (advisories, bans, licenses, sources ok). Original finding:** RUSTSEC-2026-0370 and two siblings: `zvariant`, `zvariant_derive`, `zbus_names` are "unmaintained", pulled in only on Linux through eframe → egui-winit → accesskit_winit → accesskit_unix → atspi. No upgrade exists. Not ignored, because the rules forbid ignore lists. Options for the owner: accept via `[advisories] ignore`, set `unmaintained = "workspace"` (only direct dependencies count), or wait for egui to move.
- Checks: `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0; `cargo test --workspace` 166 core + 64 app passed; `cargo deny check` exit 0 after the owner's `unmaintained = "workspace"` change (it hides unmaintained transitive crates, so the zvariant family is no longer reported).
- Not verified: the real window (this machine lacks `libxkbcommon-x11`, so `cargo run -p klefki` panics at startup), a write next to a live Claude Code session, behaviour on Windows/macOS, whether a running Claude session notices a toggle without restart, and "Claude Code is running" detection (only a generic restart warning is shown).
- Duplicate keys in `~/.claude.json` are not detected on write (the last one wins when parsed); `.mcp.json` duplicates still are.

## Engine test failure on Linux (2026-10-17)
`engine::tests::at_most_three_checks_run_at_once` failed 3 of 3 runs on Linux: `fake-claude` wrote each log line with several `write` calls, so parallel runs tore each other's lines. Not a flake. Reproducer first: `cli::runner::tests::concurrent_stub_runs_never_interleave_log_lines` (128 parallel runs) failed, then passed after the approved stub change (one `write_all` per line, `docs/proposals/fake-claude-atomic-log.md`). Evidence: `cargo test -p klefki-core --lib` 126 passed, 5 runs in a row; `cargo test --workspace` 126 core + 51 app passed; clippy and fmt exit 0. The earlier "176 passed" count is now 177 on Linux. `cargo deny` still not run.

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
- `cargo deny` runs now; advisories fail (see Docstore section).
- Theme persistence goes through eframe's storage (written by the framework to the app data folder); the app crate itself still has no fs calls. Not tested across a real restart, and flagged for review against the no-IO invariant.
- System theme follows the OS setting; only checked that it is selectable, not against a real OS theme change.
- Hover colours were only seen on one row in each theme.
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
