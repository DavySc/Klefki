# Roadmap: making Klefki more generally useful (2026-10-19)

## Goal
Record the five additions the owner chose from an evaluation of other MCP managers and clients in `docs/PLAN.md`, so later sessions can pick them up. Docs only; no code.

## Done
- `docs/PLAN.md`: new section "Roadmap: making Klefki more generally useful" after "Nice to have", ranked by impact and by how much each sets Klefki apart from other tools (owner's request): tools view; every server Claude Code loads and why one doesn't; approvals per server; server sets; other apps. Each with size, files involved, the invariant reviews it needs, and how it compares with other tools.
- Not added (not chosen): adding servers from the official MCP Registry.

## Checks
- Docs-only change; `cargo fmt --check` run to confirm the tree is untouched (see the pull request).

## Next
None of these starts until the owner says so. Rank 1 (tools view) is large and needs `net/` for HTTP servers; rank 2 is the cheapest differentiating slice, starting with a spike on how Claude Code names and turns off plugin servers and claude.ai connectors. Server sets are cheap and may be built early.
