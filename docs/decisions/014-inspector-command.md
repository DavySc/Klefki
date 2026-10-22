# 014: Copy the MCP Inspector command (2026-10-22)

## Context
Debugging proposal P6, roadmap item 10. The MCP Inspector is the protocol's own debugging tool: it lists a server's
tools, calls them and shows what comes back, which Klefki does not (the 2026-10-21 research decided against Inspector
parity). The honest hand-off is one line to paste in a terminal. Klefki never runs it: `npx` downloads and starts code,
and the Inspector keeps the values it is given in its own store.

## What the Inspector takes (2.12.0, tried: the launcher was started with each form and its `/api/config` read back)
| # | Finding |
|---|---|
| 1 | `npx @modelcontextprotocol/inspector [--cwd DIR] [-e NAME=VALUE]… -- COMMAND ARG…` opens a stdio server with the command, arguments, folder and variables filled in. The `--` is needed: without it an argument such as `-y` is read as the Inspector's own flag and dropped. |
| 2 | `npx @modelcontextprotocol/inspector --transport http\|sse --server-url URL` fills in a remote server. `--header` exists too. Without `--transport` the type is guessed from a path ending in `/mcp` or `/sse`, so Klefki always says it. |
| 3 | Version 2 needs Node 22.19 or newer; `npx` takes the latest, which is version 2. The version 1 line takes other flags (`--cli`, no `--cwd`), so the line is written for version 2 and says so. |
| 4 | The Inspector signs in to an OAuth server by itself and stores tokens, client secrets and `-e` values in the OS keychain, or in `~/.mcp-inspector/secrets.json` unencrypted where there is none. |
| 5 | It starts its web server and opens a browser; a `${VAR}` reference is not expanded. |

## Decision
- A server of the user's own files (user, local, project) that speaks stdio, http or sse has a "Copy Inspector command"
  button on the Status tab, under the last starts, and a palette entry. Plugin, organisation and claude.ai servers
  have no definition Klefki holds; WebSocket has no Inspector support: no button.
- The click sends `Command::InspectorCommand`; the engine reads the definition (like Export) and answers with the
  line and notes (`Event::InspectorCommand`). The line is copied when it arrives, shown with its notes, and the
  button says "Copied". Nothing is read or built before the click.
- No secret is typed into a shell, and so none reaches history or the Inspector's store from Klefki:
  - environment and header values are never written, whatever they look like; their names are listed to enter in the
    Inspector;
  - an argument, or an address part, that looks like a secret (the rules of Export) is written as `REPLACE_ME`, and
    a note says where;
  - `${NAME}` references are left as they are (the Inspector does not expand them) and a note says to write values in.
- A local or project server gets `--cwd` with its project folder (where Claude Code starts it), when that is a local
  absolute path; a user server gets none, and a note says to run the line from the folder you start Claude Code in
  when part of the command is relative.
- Quoting: on Unix a word that is not plain goes in single quotes. On Windows, one line has to serve PowerShell and
  `cmd`, so a word that is not plain goes in double quotes (backslashes before the closing quote doubled), and a word
  with `"`, `$`, a backtick, `%`, `^` or `!`, which the two read differently, is refused with a note to write that line
  by hand. A control or bidi character is refused everywhere: it could end the command or disguise it.
- Klefki does not run, open or download anything; the invariant that only Claude is ever spawned stands.

## Not done
- `klefki-cli inspector <name>` (with P9).
- The `--` form on PowerShell 5 and cmd: tried on Linux only. If `npx.ps1` swallowed the `--`, a server argument that
  starts with `-` would be read by the Inspector.
- Values for `-e`: left out on purpose; the Inspector's own Environment Variables box takes them.
