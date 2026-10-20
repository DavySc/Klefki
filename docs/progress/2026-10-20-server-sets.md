# Server sets, slice 1 (2026-10-20)

Goal: roadmap item 4 (the owner's "profiles"): keep named groups of servers and use one in a project. Decision 012.

## Done
- Core: `sets.rs` keeps sets in `sets.json` beside the backups folder (read size-limited and sanitised; never
  written over in part). Commands `Sets`, `SaveSet`, `DeleteSet`; events `Sets`, `SetRefused`.
- Core: `Change::UseSet` resolves to `disabledMcpServers` switches in one project or every known project, with `only`
  turning every other server there off; notes for members gone or loading only in their own project, warnings for
  unapproved project servers, what else goes off, and projects opened later. A member's name shared with another
  server stays on. `ConnectionId::shown_name` names a member that is no longer configured.
- App: a Server sets page (nav, after Connections): list with Edit and Delete (asks first), New set with a name and a
  box per server, "Use in" (Every project or a project) with "Turn off every other server there", and Review…,
  which opens the usual review. The top filter box filters sets. Read on opening the page, let go when leaving it.

## Checks
- Core tests in `sets.rs` (3), `ops.rs` (6) and `model.rs` (1); app tests in `shell.rs` (7). The core tests were
  written with the code, not seen failing first; the shared-name test was written for the bug it covers.
- Tried in the real app (Xvfb, temp `HOME` and `CLAUDE_CONFIG_DIR`, Claude Code 2.1.304): made a set, chose a project,
  ticked "Turn off every other server there", reviewed and applied. `.claude.json` then had
  `disabledMcpServers: ["notion", "filesystem"]` for that project and a backup was kept.
- Full verify (`cargo fmt --check && cargo clippy … -D warnings && cargo test --workspace && cargo deny check`): exit 0;
  core 881 passed, app 397 passed, cli 26 and 24 passed; deny: advisories, bans, licenses, sources ok.

## Next
- Palette entries and `klefki-cli` support; a set from the Connections selection.
