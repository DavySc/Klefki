# 002: Where Claude Code stores "disabled" MCP servers

Status: findings recorded 2026-10-17, Claude Code 2.1.302 on Windows 11. **Contradicts parts of PLAN.md and a CLAUDE.md invariant; see `docs/proposals/spike-findings.md`.**

## Question
Where does Claude Code's own `/mcp disable` write its state, for user, local and project (`.mcp.json`) servers? (PLAN.md, M1 day-1 spike.)

## Method
- Static: read the toggle code in the embedded JavaScript of `claude.exe`, and the usage notes the binary ships for its own model.
- Dynamic: drove the **real terminal UI** through a pseudo-terminal (`docs/decisions/spikes/pty-drive`) in a throwaway `CLAUDE_CONFIG_DIR`, with a dummy API key and pre-seeded onboarding, typing `/mcp disable <name>` and `/mcp enable <name>`.
- Reproduce: `docs/decisions/spikes/disable-toggle.ps1`.

## Findings
| # | Finding | Evidence |
|---|---|---|
| 1 | The toggle is **per project**, for every kind of server. State is the array `disabledMcpServers` in the project's entry: `~/.claude.json` -> `projects["<key>"].disabledMcpServers`. | Source: `Oo(name)` reads `projects[cwd].disabledMcpServers`; toggle writes the same array. Observed for a user-scope server, a local server and a `.mcp.json` server. The binary's own notes say the toggle "is per-project: even for a user-scope server it applies to the current project". |
| 2 | Disable **appends** the name; enable **removes** it. After the last enable the key stays as `[]`. A first toggle adds the key at the end of the project entry. | Observed sequence: `[probeU]`, `[probeU,probeL]`, `[probeU,probeL,projP]`, then removals back to `[]`. |
| 3 | `.mcp.json` servers use the **same** `disabledMcpServers` list for the toggle. Nothing was written to `.claude/settings.local.json` for the toggle itself. | Observed. This differs from PLAN.md, which says `disabledMcpjsonServers` in `settings.local.json`. |
| 4 | Approval of `.mcp.json` servers is a **separate** mechanism: `enabledMcpjsonServers` / `disabledMcpjsonServers`. In 2.1.302 the `/mcp` command moved the approval of `projP` from the project entry in `.claude.json` into `<project>/.claude/settings.local.json` (`{"enabledMcpjsonServers": ["projP"]}`). Both locations exist in the schema, so both must be read. | Observed. |
| 5 | Project key spelling written by Claude: forward slashes and an upper-case drive letter, for example `C:/Users/x/My Project`. | Observed. The real config also contains other spellings (`c:/...`, `C:\\...`). |
| 6 | `claude -p "/mcp disable x"` is refused ("aren't available in this session"). Only the interactive UI toggles, so klefki has to write the file itself. | Observed. |
| 7 | A built-in server has inverted semantics (`enabledMcpServers`). Not relevant to user servers, but it must be preserved. | Source. |

## Consequences
- A server is live in project P when: it is defined (user, local or `.mcp.json`), **and** its name is not in `projects[P].disabledMcpServers`, **and** for `.mcp.json` servers it is approved.
- A user-scope server can be on in one project and off in another. "Disable everywhere" means adding the name to every known project entry; projects first opened later are not covered (as PLAN.md already warned).
- **Current klefki code is wrong** in three places (fix is the next slice): project rows use `disabledMcpjsonServers` as the disabled list; user rows are always shown enabled; approval state is not shown at all.
- Klefki can toggle project servers **without touching the repository**: the toggle lives in the user-private `.claude.json`. Only approval (`enabledMcpjsonServers`) needs the repo's `.claude/settings.local.json`, and the existing rule stands there: show the full command before enabling a stdio server.

## Not verified
- Creating a project entry from scratch for a project Claude has never seen (which defaults Claude fills in).
- Which entry Claude reads when the same project appears under two spellings in `.claude.json`.
- The exact write format of `settings.local.json` beyond the one observed file.
- macOS and Linux, and Claude Code versions other than 2.1.302.

## Side effects of the spike
- My first scripted run pressed Enter through the API-key prompt into the login screen, which printed an OAuth URL; Claude may have tried to open a browser tab. Nothing was signed in. Later runs pre-seed onboarding so this cannot happen again.
- Notes for the M2 PTY login work: Windows ConPTY draws nothing until the host answers the cursor-position query (`ESC[6n`), and `portable-pty` + `vt100` rendered the real UI correctly.
