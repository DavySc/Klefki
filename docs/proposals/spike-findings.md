# Changes from the M1 spikes (APPLIED 2026-10-17 after the owner agreed)

The spikes (`docs/decisions/001-*.md`, `002-*.md`) contradict parts of `docs/PLAN.md` and one invariant in `CLAUDE.md`. The owner agreed, and the CLAUDE.md invariant and the PLAN.md sections below were updated. Kept for the record.

## CLAUDE.md invariant
Current:
> Never create entries in `project` scope (`Target` is `User | Local` only). Project servers are toggled only through `.claude/settings.local.json`.

Proposed (the first sentence is unchanged and still holds):
> Never create entries in `project` scope (`Target` is `User | Local` only). Project servers are toggled through `disabledMcpServers` in the project entry of `~/.claude.json`, like every other server. The repository's `.claude/settings.local.json` is written only to approve a project server (`enabledMcpjsonServers`), after showing its full command.

Why: the real `/mcp disable` never writes `settings.local.json` for the toggle (decision 002, finding 3). Toggling through `.claude.json` also keeps klefki from editing files inside repositories.

## PLAN.md, "Enable/disable semantics"
Replace the four bullets with:
- **All scopes**: toggle = membership of the name in `projects[<key>].disabledMcpServers` (append on disable, remove on enable, leave `[]`).
- **User servers**: per project. "Disable everywhere" adds the name to every known project entry, with the caveat that projects first opened later are not covered.
- **Project (`.mcp.json`) servers**: the toggle is the same list. Approval is separate: `enabledMcpjsonServers` / `disabledMcpjsonServers`, read from both the project entry and `<project>/.claude/settings.local.json`, written only to the latter.
- **Legacy `mcpServersDisabled`**: unchanged.
- Remove "Verify the exact `disabledMcpServers` location": done, see decision 002.

## PLAN.md, `docstore.rs`
Replace "It also takes Claude Code's own lock if the M1 lock experiment shows one exists" with the protocol in decision 001: take `<claude.json>.lock` (mkdir, stale after 10 s), hold it under 1 s, abort rather than write unlocked, verify about 1 s later, and serialise as 2-space JSON with LF and no trailing newline.

## Inventory code (not a proposal, a bug to fix next)
`crates/core/src/config/inventory.rs` uses `disabledMcpjsonServers` for project rows and always shows user rows enabled. Both contradict decision 002.
