# Roadmap: making Klefki more generally useful, and ranking it (2026-10-19)

## Goal
Add the five additions the owner chose from an evaluation of other MCP managers and clients to `docs/PLAN.md`, then rank the whole remaining roadmap by impact and by how much each item sets Klefki apart (owner's request). Docs only; no code.

## Done
- `docs/PLAN.md`: new section "Roadmap (ranked 2026-10-19)" after "Nice to have": a table of every item not yet built (14 ranked, plus the model-based test as unranked quality work) with size, impact, differentiation and where it is described, then the details of the five new items (every server Claude Code loads and why one doesn't; tools view; approvals per server; server sets; VS Code servers).
- Not added (not chosen): adding servers from the official MCP Registry.
- Status of existing items checked in the tree: no `drift`, `legacy` or `net` module and no `Packager.toml` in `crates/core/src` or the root, so M3 drift, legacy import, the network probe and M4 packaging are still open.

## Checks
- Docs only. `cargo fmt --check`: exit 0.

## Next
None of these starts until the owner says so. Rank 1 is packaging and release; owner swapped sets (now 4) and approvals (now 5). Rank 2 starts with a spike on how Claude Code names and turns off plugin servers and claude.ai connectors.
