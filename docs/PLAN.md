# Plan: klefki — native, cross-platform rewrite of the Electron MCP manager

## Context
The current Electron MCP manager (Electron + React + MUI, v1.0.0) is slow and inconvenient:
- Each status check starts `claude mcp get`, which takes about 3.6 s per server and repeats every 2 s while waiting for sign-in.
- The portable exe unpacks about 230 MB on every launch.
- There is no editing, duplicating is clumsy, and only HTTP/OAuth servers are shown.

The rewrite is Rust + egui. It runs on Windows, macOS and Linux and has a dark theme in a dark, desktop-app style. It keeps the original architecture's security model:
- The UI never touches files, network, secrets or processes.
- OAuth writes go through `claude mcp add-json --client-secret`.
- It never creates entries in project scope.
- Client secrets live in OS-encrypted storage, keyed by connection id.

It also fixes the security bugs found in v1.0.0: server-name injection into a temp `.cmd` file, cmd.exe argument injection, remove-then-add without rollback, unchecked `openExternal`, and racy writes to `~/.claude.json`.

Location: `klefki` (new git repo).
Reference: the v1.0.0 source, obtained by unpacking the portable exe (NSIS → `$PLUGINSDIR/app-64.7z` → `resources/app.asar` → `out/main/index.js`). Port its logic and avoid its bugs:
- `deriveEndpoints`, the application-scope regex `^[^/\s]+\/[^/\s]+$`, `diffScopes`
- `makeId` (base64url of `{s,p,n}`)
- `normalizePathKey`, `casingCheck`
- Claude CLI discovery candidates
- `mcp get` line parsing
- drift intervals 0/60/360/720/1440 (default 360)

## Environment
- MSVC is already installed. Install Rust with `winget install Rustlang.Rustup`, then use stable MSVC with edition 2024. Rust 1.95 or newer is required (egui 0.36 needs it).
- Claude is the native build at `~/.local/bin/claude.exe`.

## Workspace layout
```
klefki/
  Cargo.toml (workspace, resolver 3; release: lto=fat, codegen-units=1, strip)
  rust-toolchain.toml, deny.toml, Packager.toml
  crates/core/         all IO; no egui dependency; public surface = api.rs only
  crates/app/          egui binary; depends only on klefki_core::api
    clippy.toml        disallowed-methods: std::fs::*, process::Command::new, std::net::*, open::*  (boundary enforced in CI)
  crates/cli/          optional headless `klefki-cli` (list/status/export)
  crates/fake-claude/  test stub of the claude CLI
  fixtures/            claude.json samples, malicious .mcp.json corpus, `mcp get` outputs with ANSI
  .github/workflows/   ci.yml (fmt, clippy -D warnings, test on win/mac/linux, cargo-deny), release.yml
```

## Key crates (pin at scaffold time)
| Purpose | Crates |
|---|---|
| GUI | eframe/egui/egui_extras **0.36.1** (glow, accesskit, persistence). Stay on 0.36 until `egui-phosphor` supports 0.37. |
| Icons | egui-phosphor 0.14 |
| JSON | serde_json with `preserve_order` + `float_roundtrip` |
| Secrets | keyring-core 1.0 + windows-native / apple-native / zbus-secret-service stores |
| HTTP | ureq 3.4 with `rustls` + `platform-verifier` (OS trust store; works behind corporate TLS proxies) |
| Login | portable-pty 0.9 (ConPTY), vt100, strip-ansi-escapes |
| File watching | notify 8 + notify-debouncer-full 0.7 |
| Utilities | zeroize/secrecy, crossbeam-channel, similar (diffs), tempfile (atomic persist), directories, url, regex, tracing, rfd, open |
| Tests | insta, proptest, proptest-state-machine, assert_fs, egui_kittest (shuttle only if the engine's concurrency grows complex) |
| Packaging | cargo-packager |

## Core (`crates/core/src`)
- **`api.rs`**: `start(opts, wake_ui) -> CoreHandle { tx: Sender<Command>, rx: Receiver<Event> }`.
  - All changes follow **Prepare(op) → PlanReady (steps, per-file diffs, warnings, needs_secret, touches_shared) → Apply(plan, secret?)**.
  - A plan records the hashes of the files it touches and is rejected if a file changed underneath.
  - The UI only holds an immutable, already-sanitised `Arc<Snapshot>`.
- **`engine.rs`**: an actor thread that owns state. It uses a 4-thread job pool, at most 3 live CLI checks, and at most 4 metadata fetches. No async runtime.
- **`model/`**:
  - `ConnectionId`: the same encoding as v1, so its settings and metadata are reused.
  - `ServerName`: `^[A-Za-z0-9_-]{1,64}$`, excluding reserved names.
  - `ProjectKey`: normalised path.
  - `Transport`: Stdio / Http / Sse / Ws / Invalid, plus a map that keeps unknown fields.
  - `Target` = `User | Local(ProjectKey)` only. A project-scope target cannot be expressed in the type system.
- **`validate.rs`, `sanitize.rs`**:
  - Checks: URL scheme, env/header names, no CR/LF in values, size limits.
  - Strips ANSI, C0/C1 control characters and bidi overrides.
  - Detects secret-looking header/env values: masks them and suggests `${VAR}`.
- **`config/`**:
  - **`fsx.rs`**: the single file access layer, with an allowlist that blocks `~/.claude/.credentials.json`.
  - **`docstore.rs`**: read, then hash, then patch ops. Before writing it re-reads, and if the file changed it re-applies the ops (up to 3 times). It keeps a rotating private backup (last 20), writes via tempfile + fsync + persist, refuses symlinks, and preserves order, unknown keys and floats. It takes Claude Code's own lock, a `<claude.json>.lock` directory (stale after 10 s), holds it under 1 s, aborts rather than writing unlocked, re-reads about 1 s after writing to catch an unlocked write by Claude, and edits `.claude.json` in place (only the changed lists' bytes; the rest stays byte for byte), with backups as hard links (`docs/decisions/001-claude-json-locking.md`).
  - **`claude_json.rs`, `settings.rs`**: honour `CLAUDE_CONFIG_DIR`.
  - **`mcp_json.rs`**: untrusted parser. Max 1 MiB and 500 servers, detects duplicate keys, never panics.
  - **`inventory.rs`**: merges all scopes, marks shadowed servers (local > project > user) and casing collisions.
- **`cli/`**:
  - **`locate.rs`**: discovery as in v1. It refuses `.cmd`/`.bat`/`.ps1`; for the npm shim it runs `node cli.js` directly. Path can be overridden.
  - **`runner.rs`**: no shell, `CREATE_NO_WINDOW`, `NO_COLOR=1`, timeout + kill, output capped and sanitised. The secret is passed only in `MCP_CLIENT_SECRET`.
  - **`parse.rs`**: parses `mcp get` output.
  - **`ops.rs`**: transactional upsert:
    - **Rename / duplicate / move**: add the new entry, check it with `get`, then remove the old one if needed.
    - **In-place OAuth edit**: snapshot the entry and its secret, remove, add. If the add fails, restore the old entry; if that fails too, save the JSON to app data and show a critical dialog.
- **`pty/login.rs`**: runs `claude mcp login <name> --no-browser` in a ConPTY session.
  - A read-only terminal pane is shown, with an input line.
  - The sign-in URL found in the output must be https, with a host matching the metadata's `authorization_endpoint`.
  - Done = exit 0 plus a confirming `get`. Timeout 5 minutes; Cancel kills the process.
- **`net/`**: https only, OS certificate verifier, proxy from env, 10 s timeout, 1 MiB body limit. The metadata cache is per base URL. **`OpenUrl`** accepts https only, and egui's link opening is routed through core.
- **`status/`**:
  - Instant hints from `~/.claude/mcp-health-cache.json` and `mcp-needs-auth-cache.json`.
  - Live `get` checks run in the background, prioritising visible rows.
  - Project stdio and `headersHelper` servers are checked only on an explicit click, because checking them runs the repo's command.
- **`drift.rs`**: scheduler, diff, excluded scopes; deduplicated per base URL.
- **`secrets.rs`**: keyring service `dev.klefki`, user `conn:<ConnectionId>`.
  - If no store is available it fails hard; there is no plaintext fallback.
  - Orphaned secrets are only listed; deleting them is an explicit user action.
- **`legacy.rs`** (M3, Windows first): read-only import of v1 `secrets.json`, `connections-meta.json` and `settings.json`. The old files are never modified.
- **`ops/`**: edit, duplicate, move/copy, enable, delete, bulk, export/import, casing fix.
- **`watch.rs`**: watches the config dir, `~/.claude/`, and each project's `.mcp.json` and `.claude/` with a 300 ms debounce, then rebuilds the inventory incrementally.
- **`logging.rs`**: secret types print `***`; secrets are never logged.

### Enable/disable semantics (verified, `docs/decisions/002-disabled-servers.md`)
- **All scopes**: toggle = membership of the name in `projects[<key>].disabledMcpServers` (append on disable, remove on enable, leave `[]`). Claude writes the key with `/` separators and an upper-case drive letter.
- **User servers**: per project. "Disable everywhere" adds the name to every known project entry, with a caveat that projects first opened later are not covered.
- **Project (`.mcp.json`) servers**: the toggle is the same list. Approval is separate: `enabledMcpjsonServers` / `disabledMcpjsonServers`, read from both the project entry and `<project>/.claude/settings.local.json`, written only to the latter. Warn if it isn't covered by `.gitignore`. Approving a stdio server shows the full command first.
- **Legacy `mcpServersDisabled`**: shown read-only, with a migrate action.

## App (`crates/app/src`): Catppuccin Mocha / Latte with Klefki accents
Changed from the original look at the owner's request. Dark (Mocha) is the default; a light (Latte) theme and a System option were added. A warm-gray family (dark and light) can be chosen in Settings; panel edges use a soft divider colour.
- **`theme.rs`**: one `Palette` per theme (`DARK`, `LIGHT`) mapped to `egui::Visuals`, 6 px rounding. A top-bar dropdown offers System, Light and Dark; the choice is remembered through eframe's storage, and `--theme light|dark|system` overrides it for one run. All colours in one place:
  - **Dark surfaces** (Mocha): field `#11111B`, chrome `#181825`, panel `#1E1E2E`, raised `#313244`, alternate row `#232334`, border `#45475A`, hover `#3B3D50`
  - **Dark text**: `#CDD6F4`, muted `#A6ADC8`
  - **Dark accents**: primary mauve `#CBA6F7` (the ring), key gold `#F9E2AF`, link `#89B4FA`, focus fairy pink `#F5C2E7`, selection `#403957`
  - **Dark status**: success `#A6E3A1`, warn `#FAB387`, danger `#F38BA8`, disabled `#7F849C`, text on bright fills `#11111B`
  - **Light surfaces** (Latte): field `#FFFFFF`, chrome `#E6E9EF`, panel `#EFF1F5`, raised `#FFFFFF`, alternate row `#E9ECF1`, border `#BCC0CC`, hover `#E0E4EB`
  - **Light text**: `#4C4F69`, muted `#5C5F77`
  - **Light accents**: deeper variants of the Mocha hues, because Latte's pastels fail contrast as text: primary `#7C2FE0`, key `#8A5A00`, link `#1A66A6`, focus `#CF3F9B`, selection `#E7DFFA`
  - **Light status**: success `#26741A`, warn `#A83F05`, danger `#C40F35`, disabled `#8C8FA1`, text on bright fills `#FFFFFF`
  - **Contrast rules (tested for both themes)**: text and accent inks at least 4.5:1 on every surface, focus and primary outlines at least 3:1, text on bright fills at least 4.5:1.
  - **Type and size**: JetBrains Mono (OFL, embedded, regular + bold), body 15 px, buttons at least 40x34, table rows 36 px, nav rows 44 px.
- **Layout (desktop-app style)**:
  - Left nav menu: Connections, Scope drift, Secrets, Settings, Activity log.
  - Top bar: title, CLI version, global search, refresh.
  - Icon toolbar: Add, Edit, Duplicate, Move/Copy, Enable, Disable, Delete, Re-authorize, Refresh scopes, Export, Import.
  - Grid (`TableBuilder`): grouped by scope/project, sortable, multi-select, badges for shadowed, casing and invalid.
  - Where: All projects, or one project. In one, the grid shows what Claude Code loads there (off, or not loaded and why), and Turn on / Turn off and Space act there only, saved straight away with Undo (Ctrl/Cmd+Z) when nothing needs weighing.
  - Detail form on the right, with tabs: General, Auth, Headers/Env (masked), Raw JSON (live validation), Status/CLI output.
- **Dialogs**: plan confirmation with a unified diff, add wizard, login (PTY pane), duplicate (several local targets, default name `<name>_copy`), move/copy, import preview, secret prompt, casing fix.
- **Shortcuts**:
  - Ctrl/Cmd+F search, Ctrl+N add, Ctrl+E edit, Ctrl+D duplicate
  - Del delete, Space toggle, F5 refresh status
  - Ctrl+S apply, Esc cancel
- **Export**: header, env and secret values are replaced with `${VAR}` or redacted. A plain, non-secret-looking env setting keeps its value as a default (`${LOG_LEVEL:-debug}`); anything secret-looking never does. Project entries are exported only as user/local candidates. The block can be copied or saved to a file (the system's save dialog; `klefki-cli export --output`); a file is never replaced without asking, and Claude Code's own files and `.mcp.json` files are never written.
- **Secrets page**: per server, each secret it uses (env and header values, secret-looking arguments, address parts and settings, `headersHelper`, the OAuth client secret) and where it lives: typed into `.claude.json` or `.mcp.json` (with the `${VAR}` to use instead), filled in from an environment variable, made by a command, or in the system's secret store. Names and places only; no value reaches the UI. Read when the page is opened (the store is listed, never read), let go when it is left. Client secrets in the store that no server or kept definition uses are listed, and deleted only after the user confirms.
- **Import**: parsed as untrusted and previewed (target, conflict handling, full stdio commands, a secret prompt per OAuth row), then applied as one plan. The file comes from the system's open dialog (Import button, Ctrl+O, the palette), a typed path where there is no dialog, or a file dropped on the window; it is read like a paste (at most 256 KiB, UTF-8 or UTF-16, never a link or `.credentials.json`). A server whose name is taken in a ticked place is kept there by default; it can be replaced or imported under a free name instead. Each row says which `${VAR}` references must be set where Claude Code runs.

## Milestones
1. **M0**: rustup, workspace, CI on 3 operating systems. Themed shell (nav, top bar, toolbar, grid) fed by fixtures, to confirm the look early.
2. **M1 (MVP)**:
   - **Day 1 spikes, before writing `docstore`.** Record the results in `docs/decisions/`.
     - **Lock experiment.** Claude Code's binary bundles a lock-file library (`lockSync`, lock directories); find out whether it uses it for `~/.claude.json`.
       1. Watch the config directory while Claude Code saves its config (start a session, toggle `/mcp`).
       2. Look for a `.claude.json.lock` file or directory, and note how long it lives.
       3. Edit the file from outside while Claude Code is running, trigger a save, and check whether the outside edit survives. This shows whether Claude re-reads before writing or writes back its in-memory copy.
       - **If a lock exists:** `docstore` takes the same lock, with the same stale-lock rules.
       - **If not:** keep the hash-and-retry approach. Re-read about 1 s after each write and re-apply if Claude overwrote it. Show a "Claude Code is running" notice in the plan dialog.
     - **Disable location:** where Claude Code's own `/mcp` toggle writes `disabledMcpServers`.
   - Core: fsx, docstore, inventory (all transports and scopes), casing, hints, CLI locate/runner/parse, live status, watcher.
   - UI: detail panel, search, enable/disable and bulk actions, delete, duplicate, edit (form, raw JSON, diff).
   - Keyring secrets, v1 meta compatibility, shortcuts.
   - **Diagnose** (decision 004): on request, rerun a server's check with the client's own debug output (`claude --debug mcp --debug-file`), then show redacted, sanitised excerpts, what the server printed itself, and plain-language hints for common failures. Includes a "restart needed" notice in the apply dialog.
3. **M2**: add wizard (metadata, scope picker, free-port check), PTY login and re-authorize, move/copy, transactional upsert with rollback. Spike early: how `--no-browser` handles the callback port (paste-back input as fallback).
4. **M3**: drift watcher and refresh scopes, export/import, legacy secret import, orphaned-secrets view, optional `klefki-cli` CLI.
5. **M4**: cargo-packager in a GitHub Actions matrix:
   - Signing uses only options that cost nothing (owner's decision, 2026-10-20).
   - Windows: msi, NSIS and portable exe, signed with Authenticode through SignPath Foundation (free for open-source projects) once it accepts Klefki.
   - macOS: universal .app/.dmg with an ad-hoc signature. No Developer ID or notarisation: they need the paid Apple Developer Program, so Gatekeeper asks users to allow Klefki once.
   - Linux: deb and AppImage.
   - Every release: checksums signed with minisign, build provenance attestation and an SBOM.
   - Klefki updates itself (owner's decision, 2026-10-20), only to releases signed with the release key (decision 009).
   - Slices, in order: (1) unsigned release builds from a `v*` tag, with checksums and the MIT OR Apache-2.0 licence (`docs/RELEASING.md`); (2) provenance attestation and SBOM; (3) minisign; (4) macOS ad-hoc signature; (5) Authenticode through SignPath Foundation, which signs only projects that have already released, so it follows the first release; (6) auto-update: (6a) a signed update check with a notice, (6b) download, check against the signed checksums and install (decision 009). Slices 3 and 5 need the owner's key or account.

## Nice to have (after M4)
Ideas from an outside brainstorm, compared with this plan and checked on a real machine in `docs/decisions/003-brainstorm-comparison.md`. None of them blocks M0 to M4, and they are in rough order of value.
1. **Claude Desktop detection, read-only.** List the servers in `claude_desktop_config.json`. Check both locations: `%APPDATA%\Claude\` and the MSIX-virtualised `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude\` (the MSIX one is the real one on the owner's machine). No editing and no health check of Desktop servers (there is no CLI to delegate to); show them with their log tail instead.
2. **Bounded log tail for MCP servers.** Built as the Status tab's last starts by Claude Code (decision 013): what each start said, not the raw log. The memory criterion is met by reading only both ends of a file, at most 64 KB each; a test reads a 1 GB file that way. Show the newest log for a server (Claude Code: `%LOCALAPPDATA%\claude-cli-nodejs\Cache\<project>\mcp-logs-<server>\`), reading from the end of the file in chunks. Acceptance criteria, written before the code:
   - memory stays flat whatever the file size, proven by a test that runs the tail in a child process on a generated 1 GB file and checks peak working set grows by only a few MB, with the first lines visible quickly;
   - a single huge line without a newline, a file that grows or rotates while read, and a file held open by its writer are all handled;
   - text is sanitised like all untrusted output, and tokens, `Authorization` values and environment values are redacted by default (the logs can contain full JSON-RPC traffic), with an explicit "copy sanitised" action.
3. **Paste-to-add.** Accept an `{"mcpServers": {...}}` block or a `claude mcp add ...` command. Parse it ourselves (never run the pasted text), show the exact resolved command and require confirmation before anything is spawned, and write through `add-json` like every other add. Normalise the `/c` that Git Bash turns into `C:/` when it appears in a pasted `cmd /c` command.
4. **Scope visibility hint for the VS Code extension** ("this server is project-scoped; the extension may not see it"). Only after an experiment confirms the extension really behaves that way: it is installed on the owner's machine (2.1.301) but the claim is unverified.

5. **Network probe for HTTP servers** (decision 004): DNS, TLS (including a TLS-inspecting proxy) and the HTTP status of the URL, on the planned `net/` module, to explain what Claude reports as a bare `ECONNREFUSED` or `Needs authentication`.
6. **Relative-path lint** for user-scope stdio servers: the working directory a client uses is not your project folder, and a check run from the home folder can pass where a real session fails. Built (a badge and a note), and with it the Status tab's "Diagnose in a project": on request, the user server's diagnose run from a project's folder, refused where Claude Code would not start it there.
7. **Copy the MCP Inspector command** for a server (never run it automatically: `npx` downloads code, and the Inspector keeps OAuth tokens and `env:` values in its own store, so no secrets are passed).

Not planned, with the reasons recorded in decision 003: `cmd /c` validation (a plain `npx` entry works with the current native Claude Code), and cleanup of old MCP logs (a current install shows 14 MB in 659 files, so the reported runaway growth did not reproduce, and deleting files needs its own safety design).

## Roadmap (ranked 2026-10-19)
Everything not yet built, ranked by impact and by how much it sets Klefki apart from other MCP managers (mcpick, mcpm, MCPMate, Docker MCP Toolkit, add-mcp, mcp-scan; evaluation in the project's assessment of 2026-10-19). None starts until the owner says so; one slice per branch. Items that add a write target, a network call or a spawn need an invariant review first. Build order may differ where a cheap item unblocks others.

| # | Item | Size | Impact | Sets Klefki apart | Described in |
|---|---|---|---|---|---|
| 1 | Packaging and release (M4) | L | Very high: nobody can install Klefki without it, and the repo is going public | No, but every other item depends on it to reach users | Milestones, M4 |
| 2 | Every server Claude Code loads, and why one doesn't | M | High | Yes: no other tool shows plugin and organisation servers or explains a policy block, and Claude Code hides the block | Below |
| 3 | Tools view: tools, context cost, changed descriptions | L | High: tool definitions cost context every turn | Yes: no manager combines per-server cost with change detection (mcp-scan pins hashes as a CLI only) | Below |
| 4 | Profiles (server sets) | S–M | High | No: mcpm, MCPMate and Docker have profiles, mcpick plans them. High impact for little work | Below |
| 5 | Approvals per server | S–M | Medium-high | Yes: others leave Claude Code permission rules to hand-edited settings | Below |
| 6 | Network probe for HTTP servers | M | Medium: explains bare `ECONNREFUSED` and TLS-proxy failures | Yes: no manager diagnoses why a remote server fails. Builds `net/` together with 3 | Nice to have 5 |
| 7 | VS Code servers | M read-only, L to write | Medium-high: the other client the owner uses | No: add-mcp, mcpm, MCPMate and Docker cover it | Below |
| 8 | Bounded log tail, reframed as the last starts by Claude Code (done, decision 013) | M | Medium | Partly: MCP Router has logs; redaction and flat memory are Klefki's own | Nice to have 2, decision 013 |
| 9 | Scope drift watcher and Refresh scopes | M | Low-medium: OAuth gateway users only | Yes, but niche | Milestones, M3 |
| 10 | Copy MCP Inspector command (done, decision 014) | S | Low | No | Nice to have 7, decision 014 |
| 11 | Import: `${VAR}` value entry and leftovers | S–M | Low-medium | No | Progress file of Import |
| 12 | Per-configuration record of stored client secrets | S–M | Low: safety of Secrets page clean-up | No | Secrets page progress file |
| 13 | Legacy v1 secret and settings import | S | Low: v1 users only | No | Core, `legacy.rs` |
| 14 | VS Code extension scope hint | S | Low | No; blocked on an experiment | Nice to have 4 |

The model-based test of change operations is not ranked: it is quality work, best done alongside 2, 4 and 5, which add change operations.

### Details of the items added on 2026-10-19
- **Every server Claude Code loads, and why one doesn't (M).** Read-only rows for plugin servers (a plugin's `.mcp.json`, `mcpServers` in `plugin.json`, `.mcpb` bundles), organisation servers (`managed-mcp.json`, `managedMcpServers`) and claude.ai connectors (from the cached status, never a new `mcp list`). Mark a server "Blocked by policy" when `deniedMcpServers` matches it, or `allowedMcpServers` applies and does not, naming the rule and its settings file (matching: `serverUrl` with `*`, exact `serverCommand`, `serverName`). Plugin and managed files are untrusted input. Spike first: whether plugin servers and connectors can be turned off through `disabledMcpServers`, and under which names. Sets apart: no other tool shows plugin and organisation servers or explains a policy block, and Claude Code itself hides the block.
- **Tools view (L).** On an explicit click only, connect to a server and call `tools/list`: show each tool, an estimated token cost, and flag a tool whose description changed since it was last seen (stored hash). Feeds tool names into approvals and helps choose sets. A stdio server runs its command, so the "explicit click, full command shown first" rule of project checks applies; HTTP servers need `net/` (shared with the network probe and Copy MCP Inspector command). Sets apart: no other manager shows a server's context cost together with tool changes since approval (mcp-scan pins hashes, but only as a CLI scanner; MCPMate's inspector has no cost or change view). Biggest user pain: tool definitions cost context every turn.
- **Profiles (S–M).** Named sets of servers (the owner's name for them: profiles; decision 012), kept in app data, switched on for a project, for every project, or alone (everything else off). Each switch is one Prepare → diff → Apply plan over `disabledMcpServers`, so backups, the Activity log and restore apply. Available from the palette and `klefki-cli`. Expected: mcpick (planned), mcpm, MCPMate and Docker all have profiles. Cheap and high impact.
- **Approvals per server (S–M).** On the detail panel, show the permission rules that apply to a server (`mcp__<server>`, `mcp__<server>__*`, `mcp__<server>__<tool>`) and the file each comes from; write auto-approve, ask or block through a plan to `~/.claude/settings.json` (user) or `.claude/settings.local.json` (one project). `~/.claude/settings.json` is a new write target: invariant review first. Sets apart: other managers leave Claude Code's per-server permission rules to hand-edited settings files.
- **VS Code servers (M read-only; L to write).** A page like the Claude Desktop page for VS Code's own MCP servers (`.vscode/mcp.json` per workspace and the profile's `mcp.json`; key `servers`, `inputs` for secrets), with the Desktop page's rules (read on open, values never read, sanitised). "Copy to Claude Code" through the Import plan, and Export in VS Code's format. Writing to VS Code is a separate decision; a direct file writer changes the invariants. Other clients (Cursor, Windsurf, Codex, Gemini CLI and the like) are deferred indefinitely (owner, 2026-10-19). Expected: add-mcp, mcpm, MCPMate and Docker cover VS Code, so it does not set Klefki apart.

## Verification
- **Unit tests**:
  - name validation and reserved names, ProjectKey normalisation
  - `mcp get` parser (insta fixtures with ANSI), application-scope regex, drift diff
  - sanitize (ANSI, C1, bidi), masking heuristics, ConnectionId compatibility with v1 ids
- **Config round-trip**:
  - a patched, anonymised copy of the real `~/.claude.json` is byte-identical apart from the patched path
  - a file changed between Prepare and Apply is detected
  - backup rotation, symlink refusal, BOM/CRLF, `CLAUDE_CONFIG_DIR`
- **Malicious `.mcp.json` corpus plus proptest (never panics)**:
  - oversized/deeply nested files, 10k servers, duplicate keys
  - names containing `&|^%"!` or newlines; bidi/ANSI in strings
  - `javascript:`/`file:` URLs, `cmd /c` stdio
- **fake-claude integration tests (3 operating systems)**:
  - the secret appears only in env, never in argv
  - rollback restores the entry when add fails
  - timeouts kill the child
  - `.cmd` shims are refused
  - invalid names never reach a spawn
  - PTY login emits UrlOpened, then Done
  - non-https URLs are rejected
- **Model-based test of change operations** (`proptest-state-machine`, M2, used instead of a TLA+ spec):
  - **Reference model:** an abstract state per connection: whether the entry exists in each scope, whether its secret is stored, whether its tokens exist, plus the recovery file.
  - **Random operation sequences:** edit, rename, duplicate, move, delete and enable/disable, run against fake-claude.
  - **Fault injection:** fake-claude makes `remove`, `add` or `get` fail or time out at any step. Outside writes to the config can land between any two steps.
  - **Checked after every step:**
    - no entry is lost unless it can be recovered: it is still in the config, or in the app-data recovery file with its secret in the keyring
    - no project-scope entry is ever created
    - the secret never appears in argv
    - the config is valid JSON, with unknown keys and key order preserved
    - a model/real mismatch fails the test
  - **Shrinking:** every shrunk failing sequence is kept as a regression test.
- **UI**: egui_kittest smoke tests against a mock `CoreHandle` (navigation, grid, diff dialog, masked fields).
- **Manual checks**:
  - `cargo run -p klefki` against the real config: the list appears instantly.
  - Gateway add → login → Connected.
  - Disable/enable confirmed with `claude mcp get`.
  - Edit while Claude Code is running loses no writes.
  - Works behind a TLS-inspecting proxy.
  - Linux without secret-service shows a hard error; macOS shows the keychain prompt.
