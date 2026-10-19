# 006: `klefki-cli list`, `status` and `export`

Date: 2026-10-18. Decided without the owner (overnight, safe defaults); open to change. PLAN.md lists an optional headless `klefki-cli` (list/status/export). This covers `list` and `status`; `export` was added on 2026-10-19 (below).

## Commands
```
klefki-cli list   [NAME...] [--json]
klefki-cli status [NAME...] [--json] [--claude PATH]
klefki-cli export [NAME...]
klefki-cli help | --help | -h
klefki-cli --version | -V
```
- **`list`** reads the configuration only: no process is started, nothing is written, no network. It is the fast one.
- **`status`** asks Claude Code itself (`claude mcp get`) through the same engine the app uses, with its limits (no shell, at most 3 checks at a time, 30 s per check, output capped and sanitised).
  - Without names it checks every user and local server that is on.
  - **Servers that are off, and project servers rejected for their project, are never checked, even when named**: Claude Code does not start them, so "Connected" would answer the wrong question (review of #25).
  - **Project (`.mcp.json`) servers are checked only when named** (`klefki-cli status docs`): checking one may run the repository's own command, and the app also checks them only on an explicit click (PLAN.md, status). Unnamed, they are listed as not checked, with their command and the command that would check them. Named, `docs (from <repo>/.mcp.json) runs: <command>` goes to stderr before the check starts, as the app shows the command next to its Check button.
  - `--claude PATH` names the Claude CLI to use (the same rules apply: a real executable, never a `.cmd`, `.bat` or `.ps1`). Without it the CLI is found as in the app.
- `NAME` selects servers by name, in every scope. An unknown name (or command) is a usage error that suggests the closest one: case ignored, at most two edits and a third of the word, a swap of neighbours counting as one.
- No arguments at all prints the full help (with examples) to stderr, exit 2.

## Output
Written for people first, with `--json` for scripts (clig.dev: "Human-readable output is paramount", "Display output as formatted JSON if `--json` is passed").
- **Human:** grouped like the app's grid: `User`, then `Local in <project>` and `Project in <project>` per project, one line per server with aligned columns. `list`: name, on/off, transport, target, then notes (shadowed, name differs only by case, pending approval, off in n of m projects, OAuth). `status`: name, status, Claude's own message, and under a failure the same plain-language hint the app shows, its first line starting with the server's name (it reads on its own, line by line, as screen readers go). A summary line ends `status`. Labels say what happened: `Not checked` only when nothing ran (off, rejected, an unnamed project server, or the engine refused: a missing project folder), `Check failed` when it ran without an answer Klefki understands (timeout, odd output), `No answer` when nothing came back in time. Names sort as people count (`server-9` before `server-10`); project paths under the home folder show as `~/…`.
- Stdio arguments that look secret (a value after `--api-key`, `--token=`, `API_KEY=`…, or a credential-shaped argument such as `sk-…`, `ghp_…`, a URL with a password, a secret header in one argument as mcp-remote takes it: `Authorization: Bearer …`) show as `…`. A short flag's value (`-k X`) is shown: one letter is too ambiguous to guess. This is core's shown command line, so the app gets it too (CWE-532; review of #25).
- No colour and no other escape codes, so nothing depends on the terminal. All text is already sanitised by core (ANSI, control and bidi characters removed).
- **Progress:** `status` writes `Checked 2 of 3, waiting for <name>…` to **stderr, only when stderr is a terminal**, and clears it at the end; stdout stays clean for pipes.
- **Warnings** found while reading the configuration go to stderr as `warning: …`, also with `--json` (they are in the JSON too).
- **JSON:** one object, `{"schema": 1, "servers": [...], "warnings": [...]}`, plus `"claude_version"` (Claude Code's, not klefki-cli's) for `status`. `schema` goes up only when a field changes meaning or goes away. Field names are snake_case. In `status`, `status` is only what this run found: `connected`, `degraded`, `needs_auth`, `failed`, `pending_approval`, `not_configured`, or `not_checked` (the engine refused), `check_failed`, `no_answer`. A row that was not checked has `"checked": false`, `"status": null`, `"skipped"`: `"off"`, `"rejected"` or `"project_server"`, and `"last_known"` (Claude's own cache, or the approval state), so `select(.status == "connected")` never counts a server nobody checked.
- A closed pipe (`klefki-cli list | head -1`) ends quietly with exit 0, not with a panic.

## Exit codes
| Code | Meaning |
|---|---|
| 0 | Done. For `status`: every server checked is connected. |
| 1 | `status` found a checked server that is not connected (failed, needs auth, tools failed, pending approval, could not be checked). |
| 2 | Usage error: unknown command or option, a name that matches no server, no command at all. |
| 3 | Claude Code could not be used: not found, refused (a script shim), or no check got an answer (all timed out or gave output Klefki does not understand). A broken tool, not a broken server, so monitoring can tell them apart (clig.dev: map exit codes to the main failure modes). |

## `export` (2026-10-19)
- Prints the servers (all, or those named) as one `{"mcpServers": {…}}` block on stdout: the shape a `.mcp.json` has, and what the app's Add reads back when pasted. It is the same block the app's Export dialog shows, made by core (`export.rs`). It reads the configuration only: no process, no write.
- Redaction (PLAN.md, Export): every environment and header value becomes a `${VAR}` reference (a header keeps its `Bearer`/`Basic`/`Token` scheme); so does each argument, address part (user name and password, secret query parameters) or other setting that looks like a secret, by the same rules that hide them from a shown command line. Values already written as references stay. One value gets one variable; a different value under a name in use gets `NAME_2`; names the definitions already refer to are never reused. A variable is named after a variable, header, parameter or bare flag (`--db` gives `DB`), never after anything that could hold a value. OAuth client secrets are never in a definition, and the block says so for each OAuth server.
- The variables to set, and notes (a server left out, two servers with one name: the second is `NAME_2`; the same definition in two places is in the block once), go to stderr, so `> servers.json` holds only the block. No `--json`: the output is JSON already.
- Exit 0 when every server is in the block, 1 when one could not be read (the note says why), 2 for a name that matches no server.

## Not done here
Filtering by project or scope, colour. Ctrl+C does not yet stop running checks and the servers they started (follow-up, with #24's process-tree kill).
