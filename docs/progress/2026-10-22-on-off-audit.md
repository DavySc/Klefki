# On/off audit (2026-10-22)

Goal: audit every way Klefki turns servers (and claude.ai connectors) on and off, test against real Claude Code,
and propose slices in order. No code changed.

Done:
- `docs/proposals/on-off-audit.md`: live facts L1-L10 (Claude Code 2.1.308, Linux), 13 findings ranked by risk,
  slices S1-S7, open questions for the owner.

Checks run:
- Live spikes in a temp `HOME`/`CLAUDE_CONFIG_DIR` with three logging stdio servers; interactive session in tmux read
  through `/mcp` and `/context all`; `klefki-cli list`/`use` built from main e2f6c50.
- No cargo verify run: docs only.

Open failures:
- `klefki-cli use` inside a git worktree is refused (finding 7); reproducer belongs in slice S4.

Next: the owner approved S1-S5 in order (S3 and S5 included); S1 started in its own thread. PR #101 holds this doc; cross-review folded in.
