# Long functions into named steps (simplification A4)

Goal: slice A4 of `docs/proposals/simplification-plan.md`, with the leftovers from A1's and A2's reviews. One pull
request per file or small group. Behaviour and look unchanged: no golden file or look picture may change. Not in
this slice (behaviour changes, left for the owner): the Fix casing rename refused when both spellings are in one
place; `swap.rs` `same_settings`' stdio condition, inverted against its comment.

## Parts
1. Shell leftovers from A1: the notices above the list (old switch-off lists, kept servers) to `shell/notices.rs`
   (`legacy_notice`, `kept_notice`, `migrate_legacy`, `LegacyNotice`) with their tests in
   `shell/tests/notices.rs` (12 tests, both sections' headings kept); five theme and CLI-version tests to
   `shell/tests/top_bar.rs`; `outcome_toast`'s stacked doc comment dropped; `managed_read_only`'s doc says the
   organisation, not a plugin. `shell.rs` 6,914 to 6,324 lines. Checked by script: removed and added lines are
   the same apart from `pub(super)`, imports, module lines, the two doc comments and one signature `cargo fmt`
   joined.

## Next
2. Each dialog's `apply_event` arm into its module, so its fields can be private again; `apply_event` becomes a
   dispatch to named steps.
3. Core's long functions, file by file: `inventory::build`, `ops::use_profile`, `paste::add`, `paste::lex_with`,
   `restore::compare`, `cli args::parse`, `swap::run` last; the engine ideas from A2's review where they stay moves.

## Checks
See each pull request description.
