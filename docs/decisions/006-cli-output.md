# 006: `klefki-cli list` and `klefki-cli status`

Date: 2026-10-18. Decided without the owner (overnight, safe defaults); open to change. PLAN.md lists an optional headless `klefki-cli` (list/status/export). This covers `list` and `status`. `export` waits for its own open question (format).

## Commands
```
klefki-cli list   [NAME...] [--json]
klefki-cli status [NAME...] [--json] [--claude PATH]
klefki-cli help | --help | -h
klefki-cli --version | -V
```
- **`list`** reads the configuration only: no process is started, nothing is written, no network. It is the fast one.
- **`status`** asks Claude Code itself (`claude mcp get`) through the same engine the app uses, with its limits (no shell, at most 3 checks at a time, 30 s per check, output capped and sanitised).
  - Without names it checks every user and local server that is on. Servers that are off are shown, not checked.
  - **Project (`.mcp.json`) servers are checked only when named** (`klefki-cli status docs`): checking one runs the repository's own command, and the app also checks them only on an explicit click (PLAN.md, status). Unnamed, they are listed as not checked, with the command that would check them.
  - `--claude PATH` names the Claude CLI to use (the same rules apply: a real executable, never a `.cmd`, `.bat` or `.ps1`). Without it the CLI is found as in the app.
- `NAME` selects servers by name, in every scope. An unknown name is a usage error that names the closest match when only the case differs.

## Output
Written for people first, with `--json` for scripts (clig.dev: "Human-readable output is paramount", "Display output as formatted JSON if `--json` is passed").
- **Human:** grouped like the app's grid: `User`, then `Local in <project>` and `Project in <project>` per project, one line per server with aligned columns. `list`: name, on/off, transport, target, then notes (shadowed, name differs only by case, pending approval, off in n of m projects, OAuth). `status`: name, status, Claude's own message, and under a failure the same plain-language hint the app shows. A summary line ends `status`.
- No colour and no other escape codes, so nothing depends on the terminal. All text is already sanitised by core (ANSI, control and bidi characters removed).
- **Progress:** `status` writes `Checking n servers…` and a running count to **stderr, only when stderr is a terminal**; stdout stays clean for pipes.
- **Warnings** found while reading the configuration go to stderr as `warning: …`, also with `--json` (they are in the JSON too).
- **JSON:** one object, `{"servers": [...], "warnings": [...]}`, plus `"cli_version"` for `status`. Field names are snake_case and stable; status values are `connected`, `degraded`, `needs_auth`, `failed`, `pending_approval`, `not_configured`, `unknown`. A row that was not checked has `"checked": false` and `"skipped"`: `"off"` or `"project_server"`; its `status` is what was last known (Claude's own cache, or the approval state).
- A closed pipe (`klefki-cli list | head -1`) ends quietly with exit 0, not with a panic.

## Exit codes
| Code | Meaning |
|---|---|
| 0 | Done. For `status`: every server checked is connected. |
| 1 | `status` found a checked server that is not connected (failed, needs auth, tools failed, not checked after all), or the Claude CLI could not be found. |
| 2 | Usage error: unknown command or option, a name that matches no server. |

## Not done here
`export` (format and redaction rules are PLAN.md's, still open), filtering by project or scope, colour.
