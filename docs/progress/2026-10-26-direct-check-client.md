# Direct check: core client and environment (2026-10-26)

Goal: slice 2 of `docs/proposals/checks-without-claude.md`: the MCP stdio client (`mcp/stdio.rs`) and the
environment and program builder (`config/server_env.rs`) that start a stdio server the way Claude Code does. No app or
engine change; the engine takes it up in slice 3, with the CLAUDE.md invariant line.

## Done
- `config/server_env.rs`: Klefki's environment minus `MCP_CLIENT_SECRET`, what Claude Code removes
  (`CLAUDE_CODE_OAUTH_TOKEN`, the four `*_FILE_DESCRIPTOR` variables, every `OTEL_*` in any case); the settings
  files' `env` (user, then the project's `settings.json` and `settings.local.json` only when its trust dialog was
  accepted, then managed); Claude Code's own variables; the definition's `env`. `${NAME}` / `${NAME:-default}` filled
  in from Klefki's and the settings' variables only. The program is resolved on the final PATH by Klefki (never the
  working folder, never a relative or network PATH entry) and started by full path; a file with no `#!` line (or ELF /
  Mach-O start) is refused, as Claude Code refuses it; on Windows a `.cmd`, `.bat` or anything but `.exe`/`.com` is
  left to Claude Code (`NotStarted::NeedsShell`).
- `mcp/stdio.rs`: `server/discover` first (`2026-07-28` in `_meta`, Claude Code's client info and capabilities); with
  an answer listing `2026-07-28` plus `capabilities` and `serverInfo`, only `tools/list` with the protocol in `_meta`;
  otherwise (any other answer, or none within 3 s) `initialize` `2025-11-25`, `notifications/initialized`,
  `tools/list`. Answers are checked as Claude Code checks them. `roots/list` and `ping` from the server are answered.
  Lines over 1 MB skipped, reading queue bounded, stderr tail 16 KB, reasons and stderr sanitised with the server's own
  values hidden. Claude Code's time limit for the hello and again for the tools. Ends the whole tree (`cli/tree.rs`).
- `crates/fake-mcp-server`: test-only server, one mode per row of the proposal's matrix and of the measurements below.
- Both modules are `#[cfg(test)]` until slice 3 calls them, so nothing unused is compiled into Klefki (no lint
  suppression).

## Measured with Claude Code 2.1.310 (Linux, `claude mcp get`, throwaway `CLAUDE_CONFIG_DIR`)
- `initialize` answers that fail: protocol `1999-01-01`, no protocol version, no `capabilities`, no `serverInfo`,
  `serverInfo` without `version`. Connected: `2024-10-07`, `2025-06-18`.
- With `capabilities.tools` present, "tools fetch failed" for: tools not a list, a tool without a name, a name that is
  not text, no `inputSchema`, `inputSchema` `{}`, `"x"`, `null` or of `type` `string`, a `description` that is a
  number. Extra keys are fine, an empty list is Connected. Without `capabilities.tools`, no `tools/list` is sent.
- A numeric id answered as text (`"1"`) is accepted.
- `server/discover` answers that fall back to `initialize`: `supportedVersions` without `2026-07-28`, a result
  without `capabilities` / `serverInfo`. `["2025-11-25", "2026-07-28"]` goes the newer way.
- `roots/list` during `initialize` is answered `{"roots": [{"uri": "file://<folder>"}]}`.
- `${NAME}`: a name must be a letter or `_`, then letters, digits, `_` (`${1X}`, `${HO-ME}`, `${ HOME }` stay as
  written). `${NOPE:-a:-b}` gives `a:-b`; `${NOPE:-${HOME}}` gives `${HOME}`; the command is filled in too.
- Settings values that are not text: `5`, `true`, `null`, `[object Object]`.
- Removed before a server starts: also `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`, `CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR`
  and lower-case `otel_*`. Kept: `ANTHROPIC_API_KEY`, `CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`.
- `LC_CTYPE=C.UTF-8` is set over any `LC_CTYPE` unless `LC_ALL` is set. `CLAUDE_CODE_ENTRYPOINT` is `sdk-cli` under
  `mcp get`; the builder uses `cli`, what an interactive session sets (not measured here).
- A subfolder of a trusted project is not trusted itself (its own `.claude/settings.json` `env` did not reach the
  server).
- Open item of the proposal, the trust rule for `MCP_TIMEOUT`: `mcp get` took `MCP_TIMEOUT=1500` from an
  **untrusted** project's `settings.local.json` (a server answering after 3 s failed in 1.9-2.1 s, trusted or not), so
  `timeout::limit` reading the project's files regardless of trust matches Claude Code. Left as it is.

## Not done (by scope)
- `${VAR}` scanning is not yet shared with pre-flight: pre-flight looks names up in Klefki's environment only and also
  warns about `${1X}`-style references, which Claude Code never fills in. Changing that changes pre-flight findings,
  so it belongs with slice 3's engine work.

- Only the first `tools/list` page is read. Whether Claude Code follows `nextCursor` (and fails on a bad second
  page) is unmeasured; slice 3 or the tools view should measure it.

## Review (round 1, Opus cross-review)
- B1 fixed: a server that asks (`ping`, `roots/list`) and never reads its input filled the pipe, the write blocked
  and the check never ended (reproduced: the new test hung until killed). Writes now go through a thread with a
  bounded queue; a full queue counts as closed. Regression tests: `floodping` and the variant `floodroots`.
- N1: lines that do not start with `{` are skipped before parsing, and the read-ahead queue is 1.
- N2: a whole-number float in settings is written as JavaScript writes it (`1.0` is `1`, `-0.0` is `0`).
  Still open for slice 3: a 1 MB line that is an object costs about +49 MB while it is parsed into a `Value`
  (re-review measurement); before more checks run at once, parse into a narrow typed message instead.
- N3: commented (reader threads end with the pipe, as G10 accepts). N4: noted above.

## Checks
After merging main (#110) in: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test
--workspace && cargo deny check` exit 0 on Linux (core lib 1178 passed; deny all ok). CI green on Linux, Windows and
macOS for 865320b and c6565c2. Cross-review approved c6565c2.

## Next
Slice 3: route eligible checks to `mcp::stdio::check` in the engine, re-check anything else with `claude mcp get`,
remove the two `#[cfg(test)]` lines, add the CLAUDE.md invariant line and the decision record.
