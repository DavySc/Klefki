# Split shell.rs into modules (simplification A1)

Goal: slice A1 of `docs/proposals/simplification-plan.md`. `crates/app/src/shell.rs` (about 20,000 lines, half of
them tests) becomes one module per dialog or page, one module per pull request, while no other open pull request
touches `shell.rs`. Pure moves and visibility changes: behaviour and look unchanged, no golden file or look picture
may change. `ShellApp` keeps the routing (`apply_event`, `show`, the commands).

## Layout
- The code of a dialog or page goes to `crates/app/src/shell/<name>.rs`, declared in `shell.rs`: its state, its
  free functions and an `impl ShellApp` block with its methods. What `shell.rs` still uses is `pub(super)`;
  imports are named, not globbed, so each module shows what it depends on.
- Its tests go to `crates/app/src/shell/tests/<name>.rs`, a child of `shell::tests` like `look.rs`, so they use
  the shared helpers (`harness`, `click`, `press`, `show_rows`, …) through `use super::*` without making each one
  `pub(super)`.
- Each move is done by a script that cuts whole items by name (doc comments and attributes included) and pastes
  them unchanged; the only edits after it are visibility, imports and `cargo fmt` joining a line that now fits.
  Checked per pull request: the removed and added lines are the same lines, apart from those.

## Modules
1. Export dialog: `shell/export.rs` (state, `export_dialog`, `open_export`, `can_export`, `export_rows`, the
   file-name selection, the live-region helper) and `shell/tests/export.rs` (18 tests and their two helpers).
   `shell.rs` 19,982 to 18,724 lines.

Next, one pull request each: Move and Fix casing, Duplicate, Add and Import, the review and outcome, the detail
panel, the server list, the top bar and toolbar.

## Checks
See each pull request description.
