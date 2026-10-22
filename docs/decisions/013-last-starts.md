# 013: Last starts from Claude Code's logs (2026-10-22)

## Context
Debugging proposal P1 (roadmap item 8, the bounded log tail, reframed). Klefki's own check runs a server from the
home folder (user servers) or the project, with Klefki's environment, so "it works in Klefki" can still mean "it fails
in my session". Claude Code already logs every start of a server. The owner approved reading that folder on
2026-10-22.

## What Claude Code 2.1.308 does (measured on Linux with a throwaway `HOME`)
| # | Finding |
|---|---|
| 1 | Every start of a server, in a session or by `claude mcp get` or `list`, is logged to `<cache>/claude-cli-nodejs/<folder>/mcp-logs-<server>/<time>.jsonl`. `<cache>` is `env-paths`' cache folder: `$XDG_CACHE_HOME` or `~/.cache` on Linux (`XDG_CACHE_HOME` tried), `~/Library/Caches` on macOS, `%LOCALAPPDATA%\claude-cli-nodejs\Cache` on Windows (decision 003 saw that folder). `CLAUDE_CONFIG_DIR` does not move it. |
| 2 | `<folder>` is the folder Claude Code was started in, `<server>` the server's name, both with every character but `A-Z a-z 0-9` written as `-` (a character outside the BMP as two). A name longer than 200 characters is cut there and given `-` and `Math.abs(h).toString(36)` of the 32-bit hash `h = (h << 5) - h + unit` over its UTF-16 units: a 343-character folder gave `…-5iboez`, as Klefki computes it. So `my server.x` and `my-server-x` share a folder. |
| 3 | `<time>` is when that Claude Code started (`2026-10-22T07-32-16-377Z`): one file per run and server, a session's tool calls appended to it (with what a tool returned, in a failure). Claude Code deletes old files after its clean-up period (30 days unless set). |
| 4 | Each line is JSON: `debug` or `error` text, `timestamp`, `sessionId`, `cwd`. A start is `Starting connection with timeout of Nms`; then `Successfully connected (transport: stdio) in 62ms`, or `Connection failed after 388ms, …` and `Connection failed (CONNECTION_CLOSED): Connection closed` (`CONNECT_TIMEOUT`, `ENOENT`, `ECONNREFUSED` seen). What the server printed is `Server stderr: …`. HTTP servers log their headers with `Authorization` as `[REDACTED]`; Claude Code also redacted `token=…` in stderr. |
| 5 | A server that connected but failed listing its tools (`Connected · tools fetch failed`) logs a clean connect and close; that failure is not in the log. |

## Decision
- On the Status tab, "Last starts by Claude Code": the newest start in each folder, newest first, at most 3, each with
  when, where, connected (in how long) or failed (Claude Code's code and message, how long), what the server printed,
  and hints like Diagnose's. The failed one is open. "Copy last starts" copies the same text.
- Read when the Status tab of a server is shown (`Command::LastStarts`), again after a check of it ends (a check is a
  start too) or the files change, keeping what was shown until the answer comes. Nothing is drawn before the first
  answer, which takes milliseconds.
- Bounded: the log folder's entries (at most 5,000 folders, 20,000 file names per server folder) are listed, never
  read; of at most 8 files only the first and last 64 KB are read; at most 20 printed lines of 300 characters per
  start. Links, folders, pipes and names not shaped like Claude Code's are skipped. A line cut by where reading
  stopped, a line that is not JSON, and bytes that are not text are skipped.
- Only the lines above are used: tool calls, headers and Claude Code's own stack traces are never shown.
- Shown text is sanitised; the server's own `env` and `headers` values (6 characters or more) are hidden wherever
  they appear, then everything Diagnose redacts (tokens, `Authorization`, `key=`-style values, known token shapes).
- A local or project server's starts are those whose `cwd` is its project or a folder in it. A user server's leave
  out projects where a local or approved project server of its name loads instead. A server Claude Code does not load
  at all, or a project server hidden by a local one, says the logs are about the other server. Another configured
  server whose name is written the same in the folder name is named.
- Klefki never writes, moves or deletes these files.

## Not done
- Telling Klefki's own checks from a session: both are starts. The section's hover says Klefki checks user servers
  from the home folder; in practice the home folder's entry is often Klefki's.
- macOS and Windows were not tried live; the folder follows `env-paths` and decision 003's Windows path.
- A raw log viewer: the reframed item shows what each start said, not the file.
