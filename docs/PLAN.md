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
- MSVC is already installed. Install Rust with `winget install Rustlang.Rustup`, then use stable MSVC with edition 2024. Rust 1.88 or newer is required.
- Claude is the native build at `~/.local/bin/claude.exe`.

## Workspace layout
```
klefki/
  Cargo.toml (workspace, resolver 3; release: lto=fat, codegen-units=1, strip)
  rust-toolchain.toml, deny.toml, Packager.toml
  crates/core/         all IO; no egui dependency; public surface = api.rs only
  crates/app/          egui binary; depends only on klefki_core::api
    clippy.toml        disallowed-methods: std::fs::*, process::Command::new, std::net::*, open::*  (boundary enforced in CI)
  crates/cli/          optional headless `klefki` (list/status/export)
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
  - **`docstore.rs`**: read, then hash, then patch ops. Before writing it re-reads, and if the file changed it re-applies the ops (up to 3 times). It keeps a rotating private backup (last 20), writes via tempfile + fsync + persist, refuses symlinks, and preserves order, unknown keys and floats. It also takes Claude Code's own lock if the M1 lock experiment shows one exists.
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

### Enable/disable semantics
- **Local**: the native `projects[p].disabledMcpServers`.
- **Project**: `<p>/.claude/settings.local.json` `disabledMcpjsonServers`. Warn if it isn't covered by `.gitignore`. Enabling a stdio server shows the full command first.
- **User**: disable in all known projects, with a caveat that new projects aren't covered.
- **Legacy `mcpServersDisabled`**: shown read-only, with a migrate action.
- Verify the exact `disabledMcpServers` location with a real `/mcp` toggle on day 1 of M1.

## App (`crates/app/src`): dark look
- **`theme.rs`**: one `Palette` mapped to `egui::Visuals`, with 2 px rounding. Starting colours, all in one place for tuning:
  - **Surfaces**: background `#16191D`, panels `#1E2227` / `#252A31`, alternate row `#20252B`, border `#313842`
  - **Text**: `#E6E9ED`, muted `#9AA4AF`
  - **Brand**: primary `#1D71B8`, hover/focus/nav indicator `#38A8E0`, selection `#1C52A1`
  - **Status** (foreground / fill): success `#4CC27A` / `#1E7A3C`, warn `#F0A043` / `#B4650A`, danger `#EF5350` / `#C62828`, drift `#38A8E0`, disabled `#6B7580`
- **Layout (desktop-app style)**:
  - Left nav menu: Connections, Scope drift, Secrets, Settings, Activity log.
  - Top bar: title, CLI version, global search, refresh.
  - Icon toolbar: Add, Edit, Duplicate, Move/Copy, Enable, Disable, Delete, Re-authorize, Refresh scopes, Export, Import.
  - Grid (`TableBuilder`): grouped by scope/project, sortable, multi-select, badges for shadowed, casing and invalid.
  - Detail form on the right, with tabs: General, Auth, Headers/Env (masked), Raw JSON (live validation), Status/CLI output.
- **Dialogs**: plan confirmation with a unified diff, add wizard, login (PTY pane), duplicate (several local targets, default name `<name>_copy`), move/copy, import preview, secret prompt, casing fix.
- **Shortcuts**:
  - Ctrl/Cmd+F search, Ctrl+N add, Ctrl+E edit, Ctrl+D duplicate
  - Del delete, Space toggle, F5 refresh status
  - Ctrl+S apply, Esc cancel
- **Export**: header, env and secret values are replaced with `${VAR}` or redacted; project entries are exported only as user/local candidates.
- **Import**: parsed as untrusted and previewed (target, conflict handling, full stdio commands, a secret prompt per OAuth row), then applied as one plan.

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
3. **M2**: add wizard (metadata, scope picker, free-port check), PTY login and re-authorize, move/copy, transactional upsert with rollback. Spike early: how `--no-browser` handles the callback port (paste-back input as fallback).
4. **M3**: drift watcher and refresh scopes, export/import, legacy secret import, orphaned-secrets view, optional `klefki` CLI.
5. **M4**: cargo-packager in a GitHub Actions matrix:
   - Windows: msi, NSIS and portable exe, signed with Authenticode.
   - macOS: universal .app/.dmg, Developer ID + notarisation.
   - Linux: deb and AppImage, with checksums and minisign.
   - Build provenance attestation and an SBOM.
   - Auto-update only from an org-owned, signature-verified source.

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
