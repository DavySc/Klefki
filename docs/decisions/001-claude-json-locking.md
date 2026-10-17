# 001: Writing `~/.claude.json` next to a running Claude Code

Status: findings recorded 2026-10-17, Claude Code 2.1.302 on Windows 11. Decision proposed, not yet implemented in `config/docstore.rs`.

## Question
Does Claude Code lock `~/.claude.json`, and does it re-read the file before writing? This decides how `docstore` writes without losing either side's changes (PLAN.md, M1 day-1 spike).

## Method
- Static: searched the embedded JavaScript of `~/.local/bin/claude.exe` (a Bun bundle) for locking and config-saving code.
- Dynamic: ran real `claude mcp add|remove` commands against a throwaway `CLAUDE_CONFIG_DIR`, watching the directory and polling for the lock. The real `~/.claude.json` was only read to learn its shape, and `.credentials.json` was never opened.
- Reproduce: `docs/decisions/spikes/lock-basics.ps1` and `lock-hold.ps1`.

## Findings
| # | Finding | Evidence |
|---|---|---|
| 1 | Every config save takes a lock **directory** `<claude.json>.lock` (`.claude.json.lock` next to the file). | Source: bundled `proper-lockfile` (mkdir-based), call with `lockfilePath: "<file>.lock"`. Observed: seen as a directory in 50 of 50 polls, never as a file; removed after each save. |
| 2 | Lock parameters are the library defaults: stale after 10 s without an mtime refresh, refreshed every 5 s. | Source: `stale: 1e4`, `update` defaults to `stale / 2`. |
| 3 | Under the lock Claude **re-reads the file**, applies its change to that fresh copy, then writes. Unknown keys, key order and floats survive. | Source: save function re-reads inside the lock and merges `projects`. Observed: a key added from outside (`zzOutsideKey` with `1.5`) survived a later `claude mcp add`, and stayed last. |
| 4 | Claude waits only a short while for a lock held by someone else, then **writes without the lock**. It does not remove or respect our lock after that. | Observed: with our lock held, `mcp add` finished and wrote in 2.05 s (hold 1.5 s) and 2.13 s (lock held 30 s with its mtime refreshed every 4 s); baseline is about 0.55 s. So it gives up after roughly 1.5 s extra. The source has a `1500` constant next to the retry table, probably the budget, not proven. |
| 5 | Writes are atomic: temp file `.claude.json.tmp.<pid>.<12 hex>` in the same directory, then rename. | Observed in directory events. |
| 6 | File format: `JSON.stringify(x, null, 2)`, LF line endings, **no trailing newline**, no BOM. | Inspected the written file. `serde_json::to_string_pretty` produces the same layout. |
| 7 | One CLI command performs many saves (about 10 lock cycles for one `mcp remove`), and a session saves often. Collisions are common. | Directory events. |
| 8 | Claude also keeps rotating backups in `<config dir>/backups/.claude.json.backup.<epoch ms>`. | Observed. |
| 9 | `.storage-write.lock` guards the credential store. It is not related to `.claude.json`. | Observed and source. Never touch it. |

## Decision (proposed for `docstore`)
1. **Take the same lock**, politely: `mkdir <claude.json>.lock`. If it exists and its mtime is older than 10 s it is stale: `rmdir` it and retry. Retry with short back-off (50, 100, 200, 400, 800 ms, then 1.5 s) for up to about 3 s. If still not acquired, **abort** with "Claude Code is writing its config, try again". Never write unlocked (stricter than Claude, which does).
2. **Hold it briefly**, well under Claude's 1.5 s patience: read, hash-check against the plan, patch, write temp file in the same directory, `fsync`, rename, `rmdir`.
3. **Verify afterwards**: Claude can write unlocked after 1.5 s, so re-read about 1 s after our write and re-apply (up to 3 times) if our change is gone. This keeps the plan's hash-and-retry design; finding 4 is the reason it stays.
4. **Serialise** with 2-space pretty printing, LF, no trailing newline, so untouched parts stay byte-identical. Preserve a BOM or CRLF only if we read one.
5. **Backups**: keep klefki's own rotating backups (last 20) in its app-data directory. Do not write into Claude's `backups/`.
6. Show a "Claude Code is running" notice in the plan dialog when a `claude` process holds the config (best effort), since collisions are expected.

## Not verified
- A long-running interactive session with in-memory state: it needs a login. By source it uses the same save path (re-read under the lock; a changed mtime only produces telemetry), so an outside edit should survive its next save, but this was not exercised live.
- macOS and Linux (same JavaScript and `mkdir` semantics, but not run).
- Other Claude Code versions. The details above are 2.1.302 only, so re-check when upgrading.
