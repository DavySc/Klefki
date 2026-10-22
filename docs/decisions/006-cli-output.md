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
| 1 | `status` found a checked server that is not connected (failed, needs auth, tools failed, pending approval, could not be checked), or was given the name of a server Claude Code does not run (off, rejected for its project, not loaded). |
| 2 | Usage error: unknown command or option, a name that matches no server, no command at all. |
| 3 | Claude Code could not be used: not found, refused (a script shim), or no check got an answer (all timed out or gave output Klefki does not understand). A broken tool, not a broken server, so monitoring can tell them apart (clig.dev: map exit codes to the main failure modes). |
| 4 | `export --output` wrote nothing: the file is there and replacing it was not confirmed, or Klefki does not write there (see below). |
| 5 | `status` was given the name of a server Klefki does not check (a plugin's, the organisation's, a claude.ai connector): nothing is known about it, which is neither a pass nor a failure. |

`status` exit codes for named servers (2026-10-22, debugging proposal P9): a name on the command line is a question a script asks ("does this server work in Claude Code?"), so a named server that is off, rejected or not loaded fails the run (1) though nothing ran, and one Klefki leaves to Claude Code's `/mcp` gives 5 rather than a false 0. Without names, only the servers checked count, as before: one switched off is not a failure. When several apply, 3 (every check broke down) comes first, then 1, then 5. Before, a named server that was off exited 0. A user or local server that is not loaded because the organisation's server of the same name loads instead does not fail a run that names it: Claude Code does run a server of that name, the organisation's, which gives 5 (review of #100). Like every code but 0, 5 is a failure to `&&` and `set -e`; a script that accepts "not checked" tests for it (`[ $? -eq 5 ]`). The summary line counts `off`, `rejected` and `not loaded` apart from `not checked`, so the text says why a named run failed.

Pre-flight findings (decision of P3, `api::ServerRow::preflight`) are shown under a `status` row that is not connected, `<name>: <problem>` then `- <fix>`, after the hint, and are in the JSON as `"preflight": [{"kind", "problem", "fix"}]` (`kind`: `not_found`, `mangled_switch`, `no_input`, `unset_variable`, `empty_value`). As in the app, "not found" is left out once the server started (connected, tools failed, needs auth). `list` does not look on the PATH, so it has none.

## `export` (2026-10-19)
- Prints the servers (all, or those named) as one `{"mcpServers": {…}}` block on stdout: the shape a `.mcp.json` has, and what the app's Add reads back when pasted. It is the same block the app's Export dialog shows, made by core (`export.rs`). It reads the configuration only: no process, no write.
- Redaction (PLAN.md, Export): every environment and header value becomes a `${VAR}` reference (a header keeps its `Bearer`/`Basic`/`Token` scheme); so does each argument, address part (user name and password, secret query parameters) or other setting that looks like a secret, by the same rules that hide them from a shown command line. Values already written as references stay. One value gets one variable; a different value under a name in use gets `NAME_2`; names the definitions already refer to are never reused. A variable is named after a variable, header, parameter or bare flag (`--db` gives `DB`), never after anything that could hold a value. OAuth client secrets are never in a definition, and the block says so for each OAuth server.
- A value made entirely of references stays; one that mixes a reference with text is replaced, with a note. Secret-looking URL path segments, and a `command` or `headersHelper` holding a known key shape, are replaced too (the latter two whole). An environment value that is a plain setting (short number, word, path, or URL without credentials, under a name that does not sound secret) becomes `${NAME:-value}`, so the block works with nothing set; stderr lists those apart from the variables that must be set.
- The variables to set, and notes (a server left out, two servers with one name: the second is `NAME_2`; the same definition in two places is in the block once), go to stderr, so `> servers.json` holds only the block. No `--json`: the output is JSON already.
- Exit 0 when every server is in the block, 1 when one could not be read (the note says why), 2 for a name that matches no server.
- **`--output FILE` / `-o FILE`** (2026-10-19) writes the block to `FILE` instead of stdout (stdout stays empty), and stderr says `Saved <name or N servers> to FILE.` before the variables. The file is written whole (a temporary file beside it, then moved into place). An existing file is kept: at a terminal the CLI asks `Replace it? [y/N]`; otherwise it names `--force`, which replaces it (clig.dev: "Confirm before doing anything dangerous", with a flag for scripts). Claude Code's own files (`.claude.json`, anything in its configuration folder or a `.claude` folder) and `.mcp.json` files are refused even with `--force`: they change only through a reviewed plan, and a project's servers belong to its repository. A link is never followed. Exit **4** when the file was not written, so a script can tell it from a server left out (1).

## `profiles` and `use` (2026-10-20, decision 012; the owner chose "list and use")
- **`profiles [--json]`** lists the profiles made in Klefki, read like the app reads them: each with its servers
  (`User`, `Local in <project>`, or `no longer configured`) and the projects where its servers are the only ones on.
  It reads only. JSON: `{"schema": 1, "profiles": [{"name", "servers": [{"name", "scope", "project", "configured"}],
  "in_use": [paths], "alone_in": [paths]}], "warnings": [...]}` (`in_use` and its "In use in" line since 2026-10-22: the
  projects where stopping it would put something back); a profiles file Klefki cannot read in full is a warning, and what could
  be read is listed.
- **`use PROFILE [--project DIR | --all] [--only] [--yes]`** is the first command that changes Claude Code's
  configuration. It makes the same plan as the app's "Use profile…" (`Change::UseProfile`, Prepare → diff → Apply
  through the engine, a backup first, the Activity log) and prints it on stdout: what it does, warnings, notes, then
  each file's changes, values that may be secret hidden as in the app. Then it asks `<action>? [y/N]` at a terminal;
  with nobody there it saves nothing and names `--yes` (clig.dev: "Confirm before doing anything dangerous", a flag
  for scripts). The profile is found by name, case aside, with the closest name suggested; the project is the current
  folder's unless `--project` or `--all` says otherwise, matched as given and then with links resolved. A folder
  inside a git repository that is not a project itself is the repository's, as Claude Code takes the repository's
  root there. A folder Claude Code does not know is a usage error, as is an unknown profile: both are checked before
  anything starts, and the usage line shown is `use`'s own. The question names what is decided (the plan's summary),
  since a long list of changes scrolls it out of view.
- `use` exits 0 when saved or when nothing needs changing, 4 when nothing was saved (not confirmed), 1 when the plan
  could not be made or saved, 2 for a profile or folder that is not there. It runs no Claude Code command beyond the
  `--version` the engine asks for when it starts. `--json` is refused: the plan is for people.
- **`switch PROFILE [--project DIR] [--only] [--yes]`** and **`stop PROFILE [--project DIR | --all] [--yes]`**
  (2026-10-22, on/off plan S4) work like `use`: one plan through the engine, printed, asked about, the same exit codes
  and their own usage line. `switch` stops every profile in use in that one project and uses `PROFILE` there, as one
  change (`Change::SwitchProfile`), so stopping it later goes back to before all of them; with none in use it is
  `use`. `--all` is refused for `switch`, `--only` for `stop`. `stop` makes `Change::StopProfile` (never deletes).
  In a git worktree, the project is the main checkout's: Claude Code uses its entry there (the audit's L8).
- **`run PROFILE [--project DIR] [--print] [--yes] [--claude PATH]`** (2026-10-22, on/off plan S5) starts Claude
  Code in one project with only the profile's servers, through `--settings '{"allowedMcpServers":[…]}'`, and saves
  nothing. It first prints on stderr what that session loads there: the profile's servers that load, each one that
  would not and each other server that would, with why (`api::start_plan`). When those lists are not empty it asks
  (`[y/N]`), and with nobody at a terminal and no `--yes` it starts nothing and exits 4. `--print` prints the line to
  paste on stdout instead and starts nothing (0). No start command at all (no server of the profile can be named, or
  the organisation's `allowManagedMcpServersOnly`) is 1. Claude Code is found like `status` finds it, started with
  no shell in the project's folder, attached to the terminal; on Linux and macOS `klefki-cli` becomes it (`exec`),
  so the exit status is Claude Code's. On Windows it prints the line and exits 4 for now: waiting on Claude Code
  there, the first Ctrl+C (which reaches every process in the console) would end `klefki-cli` and leave Claude Code
  running under the shell (review of #106); a console control handler would need more `unsafe` in core, an open
  decision. Not found is 3. `--all` and `--only` are refused.
- `KLEFKI_DATA_DIR` names Klefki's own folder (backups, profiles, the Activity log) instead of the machine's, like
  `KLEFKI_MANAGED_DIR`, so tests and try-outs never touch it.

## Not done here
Filtering by project or scope, colour. Ctrl+C does not yet stop running checks and the servers they started (follow-up, with #24's process-tree kill).
