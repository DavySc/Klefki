# Deleting a profile puts its servers back: design pass (2026-10-20)

Goal: the owner asked that deleting a profile leave servers as they were before it was used, and floated a global
template profile. First a UX pass and a recommendation; build only after the owner picks.

## Done
- Read decision 012, `profiles.rs`, `ops.rs` `use_profile`, `restore.rs` and the delete dialog. Today a use records
  nothing, and delete says "Its servers stay as they are"; Restore puts back every switch, including later changes.
- Proposal: `docs/proposals/profile-put-back.md`. Recommends recording what each use flipped, in a new
  `profile-uses.json`, and offering "Put them back and delete" plus "Put back" on the card, through the usual plan;
  a switch changed since is left and named. A global template is not recommended (second source of truth, cannot
  reach projects opened later).

## Checks
- Docs only; no code changed, no checks run.

## Next
- Owner's choice (decision card in the thread), and approval for the new `profile-uses.json` write target if C.
