# Checks without `claude mcp get` (research)

Goal: find a way to check servers without starting Claude Code each time, with the same or similar guarantees, and
ask the owner to decide. Research only: no code, test or invariant changed.

## Done
- `docs/proposals/checks-without-claude.md`: today's cost measured with Claude Code 2.1.310 (native build) on Linux;
  what Claude Code sends, sets and reads when it starts a stdio server; the guarantees of today's check (G1-G14) and
  which a direct check keeps, recovers or loses; options ranked (hybrid recommended); safety of Klefki starting server
  commands; the CLAUDE.md line that would change; a slice plan with a spike first.
- New facts worth keeping (2.1.310): Claude Code sends `server/discover` before `initialize` and remembers the answer
  in `<config>/state/mcp-discover-verdicts.json`; a project's `.claude/settings*.json` `env` reaches its servers only
  when the trust dialog was accepted (P4 saw it never reach them on 2.1.308); `${VAR}` expansion sees settings `env`.

## Checks
- Measurements: scratch Python scripts against `claude mcp get`, `claude mcp list` and a direct handshake, throwaway
  `HOME` and `CLAUDE_CONFIG_DIR`; numbers in the proposal.
- No code changed, so no cargo checks run for this branch.

## Next
- Owner's decisions (end of the proposal). If the hybrid is approved: the spike (slice 1), then slices 2-4.
