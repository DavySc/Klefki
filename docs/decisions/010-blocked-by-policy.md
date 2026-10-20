# 010: Servers Claude Code's policy keeps from loading (2026-10-20)

## Context
Roadmap item 2, after plugins (008) and the organisation's servers (009): a server that `deniedMcpServers` or
`allowedMcpServers` blocks disappears from Claude Code's `/mcp` and `claude mcp list` without a word, even with
`--debug`. Klefki showed it as loaded.

## What Claude Code 2.1.304 does (spike, Linux, temp `HOME` and `CLAUDE_CONFIG_DIR`)
| # | Finding |
|---|---|
| 1 | Rules come from the user's `settings.json`, a project's `.claude/settings.json` and `.claude/settings.local.json` (in that project only), and the organisation's settings files. |
| 2 | A denied rule that matches blocks the server, whatever else says; the organisation's own servers too. |
| 3 | `allowedMcpServers` of all those files count together; a list that is there, even empty, blocks every server none of them matches. `allowManagedMcpServersOnly: true` in the organisation's files makes only the organisation's lists count (with none, nothing is held back). The organisation's own servers need no allowed rule. |
| 4 | A rule's fields must all match. `serverName`: the exact name, case and all, no wildcard (a plugin's full name). `serverCommand`: a stdio server's command and arguments, exactly. `serverUrl`: a remote server's address; `*` is any text; scheme and host are compared in lower case without a default port; the path as written (a trailing `/` counts). A pattern must start with a scheme, `*://`, or be `*`. |
| 5 | Names merge first, then policy judges the winner: a blocked local `x` does not bring back the user's `x`. Policy comes before plugins are compared: a blocked server leaves no plugin's server out. |

## Decision
- `config/policy.rs` reads the rules per place (outside every project, then each project) and says why a server is
  held back: the rule and its file, or the allowed lists' files when none lets it load.
- A server held back wherever it would load is "not loaded" (`ServerRow::not_loaded`, decision 009), with that reason;
  never checked. Held back in some projects only: `ServerRow::blocked` (the reason, the projects), a quiet badge
  "blocked in N/M" and a details row. Counted like "left out": projects, or outside them when Claude Code knows none.
- Project files are read only for a local absolute path, size-limited, like the plugin switches. All the files are
  already watched.
- Text from the rules is untrusted: shown cleaned and cut.

## Not yet
- A change's review does not yet say that policy will block the server being added or turned on.
- Policy from the Windows registry or macOS profiles, and settings a server delivers.
- `${VAR}` in a server's command or address: Claude Code expands it before matching; Klefki compares the text.

## Not verified
- Windows and macOS; Claude Code versions other than 2.1.304.
