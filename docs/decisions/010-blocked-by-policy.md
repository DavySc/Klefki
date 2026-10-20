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
| 3 | `allowedMcpServers` of all those files count together; a list that is there, even empty, blocks every server none of them matches. `allowManagedMcpServersOnly: true` in the organisation's files makes only the organisation's lists count (with none, nothing is held back). The organisation's own servers need no allowed rule, except a `managed-mcp.json` server that uses `${VAR}`. |
| 4 | A rule has exactly one key; Claude Code ignores an entry with more, even when each would match, and still reads the rest of the list. `serverName`: the exact name, case and all, no wildcard; in `allowedMcpServers` only letters, digits, `-` and `_` (so a plugin's server cannot be allowed by name), in `deniedMcpServers` any name (a plugin's full name). `serverCommand`: a stdio server's command and arguments, exactly. `serverUrl`: see 4b. |
| 4a | An allowed list judges a remote server by its `serverUrl` rules and a stdio server by its `serverCommand` rules; a `serverName` rule counts only when the list has no rule of the server's own kind. The lists of a place count together, so a project's command rule stops the user's name rules for stdio servers there. |
| 4b | `serverUrl`: `*` is any text; scheme and host in any case, the host without a trailing dot; the path as written (a trailing `/` counts); no path matches any path. Without a port, a host written out in full matches the scheme's default port only and a host with `*` every port; `:*` is any port, the default too. A pattern must start with `scheme://` or be `*`. |
| 5 | Names merge first, then policy judges the winner: a blocked local `x` does not bring back the user's `x`. Policy comes before plugins are compared: a blocked server leaves no plugin's server out. |

## Decision
- `config/policy.rs` reads the rules per place (outside every project, then each project) and says why a server is
  held back, in plain words first, then what to do, then the rule as written: "A rule in your settings blocks the
  command localx. Remove the rule to let it load. (deniedMcpServers in ~/.claude/settings.json: {…})". The
  organisation's rules end "Your administrator can change this."; a missing allowed rule says where to add one.
  Home is `~`; a project's file is named within the project, and the message says which project.
- `ServerRow::blocked` (the reason, the projects) whenever policy holds a server back. Wherever it would load, it is
  also "not loaded" (`ServerRow::not_loaded`, decision 009): never checked, the status reads "Blocked by policy" (the
  CLI's state column `blocked`). In some projects only: a quiet badge "blocked in N/M" and a details row naming them.
  Counted like "left out": projects, or outside them when Claude Code knows none.
- A server not loaded for two causes names each where it applies: "In b: <policy>. In a, a server of the same name in
  the project loads instead."
- Project files are read only for a local absolute path, size-limited, like the plugin switches. All the files are
  already watched.
- Text from the rules is untrusted: shown cleaned and cut.

## Not yet
- A change's review does not yet say that policy will block the server being added or turned on.
- Policy from the Windows registry or macOS profiles, and settings a server delivers.
- `${VAR}`: Claude Code expands both sides before matching (a server's value from its environment, a rule from a
  pinned one, and ignores an allowed `serverUrl` whose scope expansion changes); Klefki compares the text.

## Not verified
- Windows and macOS; Claude Code versions other than 2.1.304.
