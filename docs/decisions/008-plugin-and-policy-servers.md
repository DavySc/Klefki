# 008: Servers from plugins, organisation settings and claude.ai, and servers blocked by policy (2026-10-19)

## Context
Roadmap item 2: Klefki's list should be the list Claude Code loads. Servers that plugins bring were missing, and a
server blocked by `deniedMcpServers` or `allowedMcpServers` disappears from Claude Code's `/mcp` and `claude mcp list`
without a word. The plan asked for a spike first: can plugin servers and claude.ai connectors be turned off through
`disabledMcpServers`, and under which names?

## What Claude Code 2.1.304 does (spike, Linux, temp `HOME` and `CLAUDE_CONFIG_DIR`)
Plugins were installed with `claude plugin marketplace add <folder>` and `claude plugin install`, from a local
marketplace and from a plugin with a git source; each result was read back with `claude mcp list` and `mcp get`.

| # | Finding | How |
|---|---|---|
| 1 | A plugin's server is named `plugin:<plugin>:<server>`. The plugin's name is the part of its `enabledPlugins` key before `@`; the marketplace is not in the name. Names with spaces or `!` are accepted. | `mcp list` |
| 2 | **It can be turned off like any other server:** its full name in `projects[<key>].disabledMcpServers` shows "Disabled for this project". The bare server name, or `<plugin>:<server>`, does nothing. | `mcp list`, `mcp get` |
| 3 | A plugin is on when the merged `enabledPlugins` (user `settings.json`, then the project's `.claude/settings.json` and `.claude/settings.local.json`) has `"<plugin>@<marketplace>": true`. `claude plugin disable` writes `false`; `--scope local` writes the project's file, which wins there. | `settings.json` after each command |
| 4 | Where its files are read from: `plugins/installed_plugins.json` (`plugins["p@m"][].installPath`) for a remote source; for a plugin with a `./` source in a `directory` marketplace, the marketplace's folder itself (`known_marketplaces.json` `installLocation` plus the source), not the copy in `plugins/cache`. | Editing one copy, then the other |
| 5 | Its servers are those of the plugin's `.mcp.json` and of `mcpServers` in `.claude-plugin/plugin.json` (an object, a `./` path to a file, or a list of both), together. A file may hold `{"mcpServers": {...}}` or the servers alone. A name both define takes the manifest's definition. `${CLAUDE_PLUGIN_ROOT}` in a definition is the plugin's folder (finding 4), written without `./`. | `mcp list` (review of #43) |
| 6 | **Claude Code leaves a plugin's server out when a server configured by hand has the same command and arguments (environment not compared), or the same address** (scheme and host in lower case, default port and a trailing `/` dropped; path case and query kept; `http` and `sse` alike). A server that is off where Claude runs does not count. Between two plugins, the first one's server stays and the environment is compared too; an earlier plugin's server that is off in a project does not hide a later one there. Plugin servers are compared with `${CLAUDE_PLUGIN_ROOT}` expanded; servers configured by hand with their `${VAR}` expanded from Claude Code's environment. Nothing says so outside `--debug`. | `mcp list` with pairs of definitions, in two projects (review of #43) |
| 7 | `deniedMcpServers` (`serverName`, `serverCommand`, `serverUrl` with `*`) and `allowedMcpServers` work from the user's own `settings.json` too, not only from managed settings. `serverName` matches the full plugin name. A blocked server is gone from `mcp list` and `mcp get` ("No MCP server named …"). | `mcp list`, `mcp get` |
| 8 | Claude Code writes every plugin server into `mcp-needs-auth-cache.json`, a stdio one that simply failed included. | The file after `mcp list` |
| 9 | claude.ai connectors are named `claude.ai <display name>` (with ` (2)` and so on when two share a name), and the toggle checks that name in the same list. Not tried live: it needs a claude.ai sign-in. | Read in the binary |

## Decision (this slice)
- **Read-only rows** for the servers of plugins that are on in the user's `settings.json`, in a new scope `Plugin`,
  under the name Claude Code uses (finding 1); the list groups them by plugin and shows the server's own name. The
  details say which plugin and marketplace they come from.
- **No file key** for such a row, so no change, check, diagnosis, sign-in, Export or Secrets entry can name it, and
  the ops refuse it with a message saying to change it in the plugin. Klefki never passes a plugin name to `claude`:
  it is not a `ServerName`, and the CLI invariant stays as it is.
- **No status** from the needs-auth cache (finding 8): the row says "Not checked", with where its status is (`/mcp`).
- **"Left out"** where Claude Code leaves the server out (finding 6), per project: in each project, a server
  configured by hand that is loaded there, or an earlier plugin's server that is loaded there, with the same
  definition. It names a server configured by hand first, else the earlier plugin's. The target shown is the one
  Claude Code runs (`${CLAUDE_PLUGIN_ROOT}` expanded); what the plugin writes is on hover.
- The toggle state is shown as for a user server (off in N of M projects); turning it on and off is the next slice,
  now that finding 2 shows it works.
- All plugin files are untrusted: 1 MiB per file, 256 plugins, 500 servers per plugin, 16 manifest sources, paths
  from the files must stay inside the folder they are relative to, and an install path with `..` is not followed.

## Not yet (next slices)
- Plugins on only in a project's `.claude/settings.json` or `settings.local.json`, or off there (finding 3).
- Turning a plugin's server on and off through `disabledMcpServers` (finding 2), with the model-based test.
- Organisation servers (`managed-mcp.json`, `managedMcpServers`), plugins from managed settings, and `.mcpb` bundles.
- "Blocked by policy" (finding 7), naming the rule and its file.
- claude.ai connectors (finding 9), from a cache only, never a new `mcp list`.

## Not verified
- Windows and macOS; Claude Code versions other than 2.1.304. On Windows, whether Claude Code compares
  `${CLAUDE_PLUGIN_ROOT}/s.js` (a `/` after a folder with `\`) with a hand-written `...\s.js` as the same: Klefki
  compares the text, so it does not.
- A plugin from a GitHub marketplace (no network in the spike); a git source was used instead.
- Which definition wins when two of a manifest's own sources define one name (Klefki takes the later, as the manifest
  over `.mcp.json`).
- The order in which two plugins are compared (Klefki takes `enabledPlugins` order, which matched the spike).

## Known gap
- A server configured by hand with `${VAR}` in its command, arguments or address is compared by Claude Code after it
  expands the variable from its own environment (`echo ${KDUP}` with `KDUP=dup` left out a plugin's `echo dup`).
  Klefki does not know that environment, so it compares the text as written and can miss such a match. It does not
  guess from its own environment, which is not Claude Code's.
- Where a server is left out by a server configured by hand in one project and by an earlier plugin's in another, the
  details name the one configured by hand.
