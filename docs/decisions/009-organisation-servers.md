# 009: Servers your organisation sets (2026-10-20)

## Context
Roadmap item 2, after plugins (decision 008): an administrator can set MCP servers for everyone on a machine, and
can make those the only ones Claude Code loads. Klefki listed none of them, and showed the user's own servers as
loaded when Claude Code ignored them.

## What Claude Code 2.1.304 does (spike, Linux, `/etc/claude-code`, temp `HOME` and `CLAUDE_CONFIG_DIR`)
| # | Finding | How |
|---|---|---|
| 1 | The folder is `/etc/claude-code` on Linux (the binary also names `/Library/Application Support/ClaudeCode` and `C:\Program Files\ClaudeCode`). It is read whatever `CLAUDE_CONFIG_DIR` says; `CLAUDE_CODE_MANAGED_SETTINGS_PATH` did not move it for `mcp list`. | `mcp list` |
| 2 | **`managed-mcp.json`** (`{"mcpServers": {...}}`, any transport): while it exists, only the organisation's servers load. User, local, project and plugin servers are gone from `mcp list`. `mcp get` says "Scope: Enterprise config". | `mcp list`, `mcp get` |
| 3 | **`managedMcpServers`** in `managed-settings.json` takes `http` and `sse` servers only; a stdio one is dropped with a message only `--debug` shows ("a managed settings document must not name a program to run"). `mcp get` says "Scope: Managed settings (provided by your organization)". | `mcp list`, `--debug` |
| 4 | `managed-settings.d/*.json` add to `managed-settings.json`; a drop-in's server replaces one of the same name. A server of `managed-mcp.json` replaces a `managedMcpServers` one of the same name. | `mcp list` |
| 5 | An organisation's server replaces a user, a local or an approved project (`.mcp.json`) server of the same name. A server configured by hand with the same address is **not** left out (unlike a plugin's, decision 008 finding 6). | `mcp list` |
| 6 | Both kinds are turned off per project by name in `disabledMcpServers`, like every other server. | `mcp list`, `mcp get` |
| 5a | A plugin's server with the same command or address as an organisation's server is left out where that one is on, as for a server configured by hand (cross-review of #47). | `mcp list` |
| 5b | With no `.claude.json` at all, the organisation's servers still load. | `mcp list` |
| 7 | Policy (`deniedMcpServers`, `allowedMcpServers`, `allowManagedMcpServersOnly`) applies to the organisation's servers too. Next slice. | `mcp list` |

## Decision
- A new scope `Managed`, shown as **Organisation**: read-only rows like a plugin's (no file key, never checked, no
  Edit, Delete, Move, Duplicate, Export or Secrets entry), switched on and off per project by name like a user server.
  The details say which file sets it and that only an administrator changes it.
- `ServerRow::not_loaded` says why Claude Code loads a configured server nowhere: `managed-mcp.json` exists, or the
  organisation's server of the same name replaces it. The list shows a "not loaded" badge with the reason; such a
  server is never checked (`claude mcp get` would answer for the organisation's one), and it leaves no plugin's server
  out. Adding a server tells you when Claude Code will not load it, for either reason.
- The Organisation group comes first, in the app and in `klefki-cli list`: its servers win over every other of the
  same name. Its details say which servers it replaces.
- A server not loaded says so once: in the status column (app), in the state column (CLI), with the reason.
- What Claude Code ignores in the organisation's files (a stdio server in a settings file, a file it cannot read) is
  `Snapshot::organisation_notes`: a muted line in the app, `note:` in the CLI, ending "Your administrator can change
  this." It is not a warning the user can act on.
- Without `.claude.json`, Klefki still lists the organisation's and the plugins' servers.
- `Snapshot::organisation_only` names `managed-mcp.json` when it exists. An unreadable one still counts (it exists).
- All files are untrusted: 1 MiB each, at most 64 drop-ins and 500 servers; never a link (`fsx`).
- The folder is watched: both files, the drop-in folder and each drop-in.
- `KLEFKI_MANAGED_DIR` names another folder (empty: none), so tests, and anyone trying this out, never depend on what
  the machine has. `ConfigPaths::in_dir` reads no organisation folder.

## Not yet (next slices)
- "Blocked by policy" (finding 7), naming the rule and its file; it will reuse `not_loaded`.
- Policy from the Windows registry or macOS profiles, and settings a server delivers (`remote-settings.json`).
- claude.ai connectors (decision 008, finding 9).

## Not verified
- Windows and macOS folders (read from the binary only); Claude Code versions other than 2.1.304.
- Whether plugins load with no `.claude.json` (assumed: they come from `settings.json`).
