# 003: An outside brainstorm compared with what klefki is building

Status: compared 2026-10-17 on the owner's Windows machine, Claude Code 2.1.302. The adopted ideas are in `docs/PLAN.md` under "Nice to have (after M4)".

## What was checked
| Claim in the brainstorm | Check | Result |
|---|---|---|
| Claude Desktop config can be in an MSIX-virtualised path, so both paths must be detected | Looked for both files (names and structure only) | **True here.** `%APPDATA%\Claude\claude_desktop_config.json` does not exist; the MSIX one under `Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Claude\` does. Its `mcpServers` is empty and its logs have no `mcp*.log`, so none of the owner's servers are in Desktop. |
| Claude Code needs a `cmd /c` wrapper for `npx` on Windows | The owner's `azure-devops` entry is plain `npx`; searched the binary for a wrapper warning | **Not true for this version.** Claude reports it Connected, and the binary has no such warning. May hold for older npm installs (unverified). |
| Claude Code MCP logs are append-only and reached 242 GB | Measured `%LOCALAPPDATA%\claude-cli-nodejs` | **Not reproduced.** 659 files, 14 MB in total, largest file 1.0 MB, in `mcp-logs-<server>` folders. Looks chunked on this version. Folder layout matches the brainstorm. |
| The VS Code extension does not see CLI-added or project-scoped servers | Extension is installed (2.1.301) | **Unverified.** Cannot be tested headlessly. Adopted only as a hint after an experiment. |
| Budget of 0.5 s and 30 MB | Release build, measured | **Missed, and 30 MB is unrealistic for egui on Windows.** 0.7 to 0.8 s to a window (1.3 s cold), 109 to 117 MB working set (75 to 82 MB private), 8.8 MB executable. The v1 being replaced spent about 3.6 s per status check and unpacked 230 MB on every launch. Better budgets: list visible from cache within a second, no UI stalls, bounded memory for anything that reads files. |
| A health check should spawn the way the client does | Our checks run `claude mcp get` | **Already true for Claude Code by delegation**, so there is no PATH or environment divergence. Not possible for Desktop (no CLI), so Desktop gets log tails, not spawns. |

## What the brainstorm misses
- OAuth add and login and keyring-held client secrets (how `REMOTE_1` and `REMOTE_2` work); Desktop's file probably cannot hold remote OAuth servers (unverified).
- Per-project toggling and `.mcp.json` approval (decision 002).
- The config-write race: Claude writes without the lock after about 1.5 s (decision 001), so "conflict detection on save" is not enough.

## Adopted (nice to have, after M4)
Read-only Desktop detection, a bounded and redacted log tail, paste-to-add through `add-json`, and the VS Code visibility hint after an experiment.

## Not adopted
- **`cmd /c` validation:** contradicted above; it would flag or rewrite working entries.
- **Log cleanup:** no evidence of the problem on a current install, and deletion needs its own prepare, preview and apply design.
